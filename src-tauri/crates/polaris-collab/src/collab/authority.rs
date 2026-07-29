//! collab/authority.rs —— 账号权威(云机签发身份断言,成员主机离线验签)。
//!
//! 背景:此前每台主机各自持有一份 `users` 表,同一个人在桌面、NAS、云机上是三个毫无关系的
//! 账号 —— 「一个账号在哪台机器都能登」做不到。本模块把**云机立为账号权威**,其余主机
//! 退化为「认签名的成员」。
//!
//! 三种运行模式(由环境变量决定,默认保持老行为,老部署零影响):
//!  · `POLARIS_ACCOUNT_AUTHORITY=1`      本进程**就是**权威。持签名私钥,`/api/account/*` 对外
//!                                        发号:注册(强制绑邮箱)、登录、改密,签发身份断言。
//!  · `POLARIS_ACCOUNT_AUTHORITY_URL=…`  本进程是**成员主机**。不再自己判密码,只认权威签的断言;
//!                                        权威公钥首次接触即钉死(TOFU),此后**离线也能验**。
//!  · 都不设                              本地权威(老行为):自己的 users 表说了算。
//!
//! 为什么是「签名断言」而不是「每次请求问云端」:成员主机大多在家里/NAS,网络随时会断。
//! 断言由**客户端**从云机取来、递给主机,主机纯本地验签 —— 主机自己不需要连得上云机。
//! 断言 TTL 只有 5 分钟:它不是会话,只是「我是谁」的一次性证明,进门即换成主机本地会话。
//!
//! 账号的全局身份是 `uid`(权威签发的稳定串),**不是**用户名,更不是各主机各自的 `users.id` —
//! 用户名将来可改,uid 不会。成员主机的 `users` 行退化成「该账号**在本机的**成员资格」:
//! 存角色、存本机的项目关系,密码位写死一个永不可验的哨兵串。

use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use std::path::PathBuf;

use super::auth::User;
use super::db::{self, now, open_db};

/// 断言有效期:只够「取到手 → 递给主机」这一趟,不当会话使。
const ASSERTION_TTL: i64 = 300;
/// 时钟偏移容忍:两台机器的时间不会分毫不差。
const CLOCK_SKEW: i64 = 120;
/// 成员主机侧联系权威的超时(取公钥/代登录)。连不上要快速失败,别把登录页吊死。
const UPSTREAM_TIMEOUT_SECS: u64 = 10;

const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// 联邦账号在本机 `users.pass_hash` 位上的哨兵。**不是**合法 PHC 串,
/// `verify_password` 见到它必定返回 false —— 即「联邦账号在成员主机上永远无法用密码登进来」,
/// 只能走权威签的断言。这是故意的:密码只有权威见得到。
pub const FEDERATED_SENTINEL: &str = "!federated";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// 本进程就是账号权威(云机)。
    Authority,
    /// 本进程是成员主机,账号权威在这个 URL。
    Delegated(String),
    /// 本地权威(老行为)。
    Local,
}

/// 当前模式。两个环境变量都设时以 Authority 优先 —— 权威机不该再去问别人。
pub fn mode() -> Mode {
    let is_authority = std::env::var("POLARIS_ACCOUNT_AUTHORITY")
        .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if is_authority {
        return Mode::Authority;
    }
    match std::env::var("POLARIS_ACCOUNT_AUTHORITY_URL") {
        Ok(u) if !u.trim().is_empty() => Mode::Delegated(u.trim().trim_end_matches('/').to_string()),
        _ => Mode::Local,
    }
}

pub fn is_authority() -> bool {
    mode() == Mode::Authority
}

/// 成员主机模式下的权威地址(其余模式 None)。
pub fn upstream_url() -> Option<String> {
    match mode() {
        Mode::Delegated(u) => Some(u),
        _ => None,
    }
}

// ── 签名私钥(仅权威机持有)────────────────────────────────────────────────

fn signing_key_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("POLARIS_ACCOUNT_KEY") {
        if !p.trim().is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    directories::UserDirs::new().map(|u| {
        u.home_dir()
            .join("Polaris")
            .join("data")
            .join("account_authority.key")
    })
}

/// 取(或首次生成)账号签名私钥。32 字节种子,base64url 落盘,unix 下 0600。
/// 与 host.key / gateway.key 分开保管:那两把是**设备/传输**身份,这把是**账号**身份,
/// 泄露后果不同(这把泄了等于谁都能冒充任何账号),故独立一把、独立轮换。
fn signing_key() -> Result<SigningKey, String> {
    let path = signing_key_path().ok_or("找不到用户目录,无法存放账号签名密钥")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建数据目录失败: {e}"))?;
    }
    if let Ok(raw) = std::fs::read_to_string(&path) {
        let seed = B64
            .decode(raw.trim())
            .map_err(|e| format!("账号签名密钥损坏(不是 base64): {e}"))?;
        let seed: [u8; 32] = seed
            .try_into()
            .map_err(|_| "账号签名密钥损坏(长度不是 32 字节)".to_string())?;
        return Ok(SigningKey::from_bytes(&seed));
    }
    let mut seed = [0u8; 32];
    getrandom::getrandom(&mut seed).map_err(|e| format!("生成账号签名密钥失败: {e}"))?;
    std::fs::write(&path, B64.encode(seed)).map_err(|e| format!("写账号签名密钥失败: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(SigningKey::from_bytes(&seed))
}

/// 权威公钥(base64url)。成员主机钉的就是这一串。
pub fn public_key_b64() -> Result<String, String> {
    Ok(B64.encode(signing_key()?.verifying_key().to_bytes()))
}

/// 公钥指纹:sha256(pubkey) 前 8 字节 hex。用来人眼核对「钉的是不是同一把钥匙」。
pub fn kid_of(pub_b64: &str) -> String {
    use sha2::{Digest, Sha256};
    let bytes = B64.decode(pub_b64).unwrap_or_default();
    let d = Sha256::digest(&bytes);
    d[..8].iter().map(|b| format!("{b:02x}")).collect()
}

pub fn kid() -> Result<String, String> {
    Ok(kid_of(&public_key_b64()?))
}

// ── 身份断言 ──────────────────────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct Claims {
    /// 签发者标识(权威公钥指纹)。
    pub iss: String,
    /// 全局账号 id —— 跨主机认人**只认这个**。
    pub uid: String,
    pub username: String,
    /// 绑定邮箱。权威强制每个账号都有,故这里必非空。
    pub email: String,
    #[serde(default)]
    pub display_name: String,
    pub iat: i64,
    pub exp: i64,
}

/// 签一张身份断言(权威机专用)。格式 `PA1.<b64u(claims)>.<b64u(sig)>`,
/// 签名覆盖 `PA1.<b64u(claims)>` 的 ASCII 字节(前缀纳入签名,防跨版本重放)。
pub fn sign_assertion(uid: &str, username: &str, email: &str, display_name: &str) -> Result<String, String> {
    let sk = signing_key()?;
    let t = now();
    let claims = Claims {
        iss: kid_of(&B64.encode(sk.verifying_key().to_bytes())),
        uid: uid.into(),
        username: username.into(),
        email: email.into(),
        display_name: display_name.into(),
        iat: t,
        exp: t + ASSERTION_TTL,
    };
    let payload = serde_json::to_vec(&claims).map_err(|e| format!("断言序列化失败: {e}"))?;
    let head = format!("PA1.{}", B64.encode(payload));
    let sig = sk.sign(head.as_bytes());
    Ok(format!("{head}.{}", B64.encode(sig.to_bytes())))
}

/// 用给定公钥验一张断言。纯本地运算 —— **不联网**,这正是成员主机断网仍能放人进来的原因。
pub fn verify_assertion_with(assertion: &str, pub_b64: &str) -> Result<Claims, String> {
    let mut it = assertion.trim().splitn(3, '.');
    let (p0, p1, p2) = match (it.next(), it.next(), it.next()) {
        (Some(a), Some(b), Some(c)) => (a, b, c),
        _ => return Err("身份断言格式不对".into()),
    };
    if p0 != "PA1" {
        return Err("身份断言版本不认识(须 PA1)".into());
    }
    let pk_bytes: [u8; 32] = B64
        .decode(pub_b64)
        .map_err(|_| "权威公钥不是 base64".to_string())?
        .try_into()
        .map_err(|_| "权威公钥长度不对".to_string())?;
    let vk = VerifyingKey::from_bytes(&pk_bytes).map_err(|e| format!("权威公钥无效: {e}"))?;
    let sig_bytes: [u8; 64] = B64
        .decode(p2)
        .map_err(|_| "断言签名不是 base64".to_string())?
        .try_into()
        .map_err(|_| "断言签名长度不对".to_string())?;
    let sig = Signature::from_bytes(&sig_bytes);
    let head = format!("{p0}.{p1}");
    vk.verify(head.as_bytes(), &sig)
        .map_err(|_| "身份断言签名验证失败(可能被篡改,或权威换了钥匙)".to_string())?;
    let claims: Claims = serde_json::from_slice(
        &B64.decode(p1)
            .map_err(|_| "断言载荷不是 base64".to_string())?,
    )
    .map_err(|e| format!("断言载荷解析失败: {e}"))?;
    let t = now();
    if claims.exp + CLOCK_SKEW < t {
        return Err("身份断言已过期,请重新登录".into());
    }
    if claims.iat - CLOCK_SKEW > t {
        return Err("身份断言的签发时间在未来(检查两端系统时钟)".into());
    }
    if claims.uid.trim().is_empty() {
        return Err("身份断言缺 uid".into());
    }
    Ok(claims)
}

// ── 成员主机侧:钉住权威公钥(TOFU)────────────────────────────────────────

/// 已钉住的权威 (url, pub_b64, kid)。没钉过返回 None。
pub fn pinned() -> Option<(String, String, String)> {
    let url = db::meta_get("authority_url")?;
    let pk = db::meta_get("authority_pub")?;
    if url.is_empty() || pk.is_empty() {
        return None;
    }
    let kid = db::meta_get("authority_kid").unwrap_or_else(|| kid_of(&pk));
    Some((url, pk, kid))
}

/// 向权威取公钥。只在需要钉/换钉时调用,登录热路径不走这里。
fn fetch_public_key(url: &str) -> Result<String, String> {
    let ep = format!("{}/api/account/pubkey", url.trim_end_matches('/'));
    let resp = ureq::get(&ep)
        .timeout(std::time::Duration::from_secs(UPSTREAM_TIMEOUT_SECS))
        .call()
        .map_err(|e| format!("连不上账号权威 {url}: {e}"))?;
    let v: serde_json::Value = resp
        .into_json()
        .map_err(|e| format!("账号权威返回的不是 JSON: {e}"))?;
    let pk = v
        .get("publicKey")
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string();
    if pk.is_empty() {
        return Err("账号权威没返回公钥 —— 确认该地址开了 POLARIS_ACCOUNT_AUTHORITY=1".into());
    }
    Ok(pk)
}

/// 确保已钉住当前配置的权威公钥,返回 (url, pub_b64)。
///
/// 首次接触即钉死(TOFU)。此后**公钥变了一律拒绝**,不静默接受 —— 静默接受等于把
/// 「中间人换一把自己的钥匙」变成无声事件,那整套验签就白做了。真要换钥匙(权威轮换密钥、
/// 换了云机)走 `repin`,由 owner 显式确认。
pub fn ensure_pinned() -> Result<(String, String), String> {
    let url = upstream_url().ok_or("本机未配置账号权威(POLARIS_ACCOUNT_AUTHORITY_URL)")?;
    if let Some((pinned_url, pk, _)) = pinned() {
        if pinned_url == url {
            return Ok((url, pk));
        }
        // 换了权威地址:同样按「显式确认」处理,不静默改钉。
        return Err(format!(
            "本机已钉在账号权威「{pinned_url}」,与当前配置「{url}」不一致 —— 请 owner 走「重新信任」确认后再切换"
        ));
    }
    let pk = fetch_public_key(&url)?;
    let k = kid_of(&pk);
    db::meta_set("authority_url", &url)?;
    db::meta_set("authority_pub", &pk)?;
    db::meta_set("authority_kid", &k)?;
    db::audit("system", "authority.pin", &url, &k);
    Ok((url, pk))
}

/// 用**显式给的地址**钉住权威(不看环境变量)。
///
/// 为什么需要它:`ensure_pinned` 的地址来自 `POLARIS_ACCOUNT_AUTHORITY_URL`,而桌面 App 是
/// 双击启动的 —— 那个环境变量根本不存在。于是桌面主机永远钉不上权威,也就**永远不接受
/// 身份断言**:同账号的另一台设备连过来会吃一句「本机未配置云端账号中心」。设备网入网时
/// 用户亲手填了地址、输了密码,正是钉住它的合法时机。
///
/// 语义与 `ensure_pinned` 一致的那一半:已经钉过**同一个** URL 就直接复用;钉过**别的**
/// URL 一律拒绝,不静默改钉 —— 静默换钥匙等于把验签这道防线拆了。真要换走 [`repin`]。
pub fn pin_explicit(url: &str) -> Result<(String, String), String> {
    let url = url.trim().trim_end_matches('/').to_string();
    if url.is_empty() {
        return Err("账号中心地址不能为空".into());
    }
    if let Some((pinned_url, pk, _)) = pinned() {
        if pinned_url == url {
            return Ok((url, pk));
        }
        return Err(format!(
            "本机已信任账号中心「{pinned_url}」,与这次要用的「{url}」不一致 —— \
             要换请走「重新信任」并核对公钥指纹"
        ));
    }
    let pk = fetch_public_key(&url)?;
    let k = kid_of(&pk);
    db::meta_set("authority_url", &url)?;
    db::meta_set("authority_pub", &pk)?;
    db::meta_set("authority_kid", &k)?;
    db::audit("system", "authority.pin", &url, &k);
    Ok((url, pk))
}

/// owner 显式重钉(换云机/轮换密钥)。要求调用方已核过 kid。
pub fn repin(expect_kid: Option<&str>) -> Result<(String, String), String> {
    let url = upstream_url().ok_or("本机未配置账号权威(POLARIS_ACCOUNT_AUTHORITY_URL)")?;
    let pk = fetch_public_key(&url)?;
    let k = kid_of(&pk);
    if let Some(want) = expect_kid {
        if !want.trim().is_empty() && want.trim() != k {
            return Err(format!("公钥指纹对不上:权威给的是 {k},你要确认的是 {want}"));
        }
    }
    db::meta_set("authority_url", &url)?;
    db::meta_set("authority_pub", &pk)?;
    db::meta_set("authority_kid", &k)?;
    db::audit("owner", "authority.repin", &url, &k);
    Ok((url, pk))
}

/// 验一张断言(成员主机侧:用钉住的公钥)。
///
/// 还没钉过就先钉一次(首次接触要连一下权威取公钥)。**只有这一次需要网络** ——
/// 钥匙钉下之后本机就能独立验签,权威挂了、家里断网了都不影响成员登录。
pub fn verify_assertion(assertion: &str) -> Result<Claims, String> {
    let pk = match pinned() {
        Some((_, pk, _)) => pk,
        None => match ensure_pinned() {
            Ok((_, pk)) => pk,
            Err(e) => {
                return Err(if upstream_url().is_some() {
                    format!("本机首次信任云端账号中心需要连一次它:{e}")
                } else {
                    "本机未配置云端账号中心(POLARIS_ACCOUNT_AUTHORITY_URL),不接受身份断言".into()
                })
            }
        },
    };
    verify_assertion_with(assertion, &pk)
}

/// 成员主机代客户端向权威登录(客户端只连得上主机、连不上云机时的兜底路径)。
/// 返回权威签的断言。
pub fn upstream_login(username: &str, password: &str) -> Result<String, String> {
    let (url, _) = ensure_pinned()?;
    let ep = format!("{url}/api/account/login");
    let resp = ureq::post(&ep)
        .timeout(std::time::Duration::from_secs(UPSTREAM_TIMEOUT_SECS))
        .send_json(serde_json::json!({"username": username, "password": password}));
    match resp {
        Ok(r) => {
            let v: serde_json::Value = r
                .into_json()
                .map_err(|e| format!("账号权威返回的不是 JSON: {e}"))?;
            v.get("assertion")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string())
                .ok_or_else(|| "账号权威没返回身份断言".into())
        }
        // 权威给的是业务错误(密码错/账号停用)——原样透传,别糊成「网络错误」误导用户。
        Err(ureq::Error::Status(_, r)) => {
            let v: serde_json::Value = r.into_json().unwrap_or_default();
            Err(v
                .get("error")
                .and_then(|x| x.as_str())
                .unwrap_or("登录被账号权威拒绝")
                .to_string())
        }
        Err(e) => Err(format!("连不上账号权威: {e}")),
    }
}

// ── 成员主机侧:断言 → 本机成员资格 ────────────────────────────────────────

/// 生成全局账号 id。`acct_` + 16 字节随机 base64url。
pub fn new_uid() -> String {
    let mut buf = [0u8; 16];
    getrandom::getrandom(&mut buf).expect("getrandom");
    format!("acct_{}", B64.encode(buf))
}

/// 入伙资格:一张有效断言只证明「你是谁」,**不证明「这台主机欢迎你」**。
/// 两者必须分开 —— 否则任何人在云端注册个账号,就能登进别人家的主机。
/// 而主机上的成员能建项目、push 仓库、触发 checks 跑构建脚本 = 主机 RCE,
/// 这正是 collab_signup 默认关闭要防的事,联邦登录不能把这道门重新打开。
pub enum Admission {
    /// 日常登录:只认已经是本机成员的账号;不是就拒(唯一例外见 upsert_member 里的空库分支)。
    ExistingOnly,
    /// 拿着 owner 签发的一次性邀请票据来入伙,角色由票据决定。
    Invited(String),
}

/// 把一张已验过的断言落成本机的成员资格,返回本机 `users` 行。
///
/// 四种情况:
///  · uid 已在本机 → 更新用户名/邮箱/昵称(权威改名后本机跟着改),复用原角色。
///  · uid 不在本机、本机零账号 → 建号并给 **owner**(装这台机器的人就是它的主人)。
///  · uid 不在本机、带邀请票据 → 按票据角色建号入伙。
///  · uid 不在本机、无票据 → **拒**。云端有账号 ≠ 本机欢迎你。
///
/// 用户名撞车(同名但 uid 不同)会明确报错交给 owner 处理,绝不静默顶替 ——
/// 顶替等于把别人在本机的项目关系送给来人。
pub fn upsert_member(claims: &Claims, admission: Admission) -> Result<User, String> {
    let mut conn = open_db()?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| format!("成员资格事务失败: {e}"))?;

    let existing: Option<(i64, String)> = tx
        .query_row(
            "SELECT id,role FROM users WHERE uid=?1",
            params![claims.uid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| format!("查成员资格失败: {e}"))?;

    if let Some((id, role)) = existing {
        let disabled: i64 = tx
            .query_row("SELECT disabled FROM users WHERE id=?1", params![id], |r| {
                r.get(0)
            })
            .map_err(|e| format!("查账号状态失败: {e}"))?;
        if disabled != 0 {
            return Err("你的账号在本主机上已被停用".into());
        }
        tx.execute(
            "UPDATE users SET username=?1, email=?2, display_name=?3 WHERE id=?4",
            params![
                claims.username,
                claims.email,
                claims.display_name.trim(),
                id
            ],
        )
        .map_err(|e| {
            if e.to_string().contains("UNIQUE") {
                // 权威那边把邮箱/用户名改成了本机另一个账号已占的值。本机不猜、不顶替。
                "同步账号资料失败:用户名或邮箱与本机另一个账号冲突,请联系 owner 处理".to_string()
            } else {
                format!("同步账号资料失败: {e}")
            }
        })?;
        tx.commit().map_err(|e| format!("提交成员资格失败: {e}"))?;
        return Ok(User {
            id,
            username: claims.username.clone(),
            role,
            display_name: claims.display_name.trim().into(),
            disabled: false,
        });
    }

    // 同名不同 uid → 拒绝顶替。
    let clash: Option<String> = tx
        .query_row(
            "SELECT uid FROM users WHERE username=?1 COLLATE NOCASE LIMIT 1",
            params![claims.username],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("查用户名失败: {e}"))?;
    if let Some(other_uid) = clash {
        return Err(if other_uid.is_empty() {
            format!(
                "本机已有同名本地账号「{}」——请 owner 先把它改名或迁移,再用云端账号登录",
                claims.username
            )
        } else {
            format!("本机已有另一个云端账号占用用户名「{}」", claims.username)
        });
    }

    let n: i64 = tx
        .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    // 空库 = 这台主机刚装好还没主人,第一个用云端账号登进来的人就是它的 owner
    // (等价于原来的 bootstrap,只是身份改由云端签)。此后一律要票据。
    let role = if n == 0 {
        "owner".to_string()
    } else {
        match admission {
            Admission::Invited(r) => r,
            Admission::ExistingOnly => {
                return Err(
                    "本主机还没有邀请你 —— 请向这台主机的管理者要一张邀请码,再用它入伙".into(),
                )
            }
        }
    };
    tx.execute(
        "INSERT INTO users(username,pass_hash,role,display_name,created_at,email,uid) \
         VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![
            claims.username,
            FEDERATED_SENTINEL,
            role,
            claims.display_name.trim(),
            now(),
            claims.email,
            claims.uid
        ],
    )
    .map_err(|e| format!("建成员资格失败: {e}"))?;
    let id = tx.last_insert_rowid();
    tx.commit().map_err(|e| format!("提交成员资格失败: {e}"))?;
    db::audit(&claims.username, "member.join", &claims.uid, &role);
    Ok(User {
        id,
        username: claims.username.clone(),
        role,
        display_name: claims.display_name.trim().into(),
        disabled: false,
    })
}

/// 断言 → 本机会话(成员主机的登录出口)。验签 → 认成员资格 → 签本机会话 token。
/// 只放已经是本机成员的人(空库首登除外);陌生账号要走 `join_with_ticket`。
pub fn login_with_assertion(assertion: &str, device_id: &str) -> Result<(User, String), String> {
    let claims = verify_assertion(assertion)?;
    let user = upsert_member(&claims, Admission::ExistingOnly)?;
    let token = super::auth::issue_session(user.id, device_id)?;
    db::audit(&user.username, "auth.login.federated", device_id, &claims.uid);
    Ok((user, token))
}

/// 云端账号 + 一次性邀请票据 → 成为本机成员并拿到本机会话。
///
/// 票据的原子核销与成员资格的创建**必须**在同一事务里,否则并发两个人拿同一张票据
/// 都能进来。这里的做法与 identity::redeem_ticket 一致:先 CAS 占用票据(`WHERE
/// used_at IS NULL`),占不到就是被人抢先了,直接拒。
pub fn join_with_ticket(
    assertion: &str,
    code: &str,
    device_name: &str,
    node_id: &str,
) -> Result<(User, String), String> {
    let claims = verify_assertion(assertion)?;
    // 已经是成员就不必再烧票据,直接当登录处理(重复点「入伙」不该白白作废一张票)。
    if let Ok(u) = upsert_member(&claims, Admission::ExistingOnly) {
        let token = super::auth::issue_session(u.id, node_id)?;
        return Ok((u, token));
    }
    let code = code.trim();
    if code.is_empty() {
        return Err("请填写邀请码".into());
    }
    let role = burn_ticket(code, &claims.username)?;
    let user = upsert_member(&claims, Admission::Invited(role.clone()))?;
    // 设备白名单:入伙即登记本设备(隧道层准入的那一因子)。失败不阻断入伙。
    let nid = node_id.trim();
    if !nid.is_empty() {
        let name = if device_name.trim().is_empty() {
            "新设备"
        } else {
            device_name.trim()
        };
        let _ = super::identity::add_device(user.id, name, nid);
    }
    let token = super::auth::issue_session(user.id, nid)?;
    db::audit(&claims.username, "ticket.redeem.federated", code, &role);
    Ok((user, token))
}

/// 原子核销一张邀请票据,返回它授予的角色。与 identity::redeem_ticket 同口径:
/// 只认 collaborator/visitor(旧的 member 归一为 collaborator),owner 票据不存在。
fn burn_ticket(code: &str, used_by: &str) -> Result<String, String> {
    let mut conn = open_db()?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| format!("兑换事务失败: {e}"))?;
    let (mut role, expires_at, used_at) = tx
        .query_row(
            "SELECT role,expires_at,used_at FROM tickets WHERE code=?1",
            params![code],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                ))
            },
        )
        .map_err(|_| "票据无效".to_string())?;
    if role == "member" {
        role = "collaborator".into(); // 兼容旧版 UI 签出的票据
    }
    if !matches!(role.as_str(), "collaborator" | "visitor") {
        return Err("该票据角色已停用,请管理员重新签发最小权限票据".into());
    }
    if used_at.is_some() {
        return Err("票据已被使用".into());
    }
    if expires_at < now() {
        return Err("票据已过期".into());
    }
    let claimed = tx
        .execute(
            "UPDATE tickets SET used_at=?1, used_by=?3 WHERE code=?2 AND used_at IS NULL",
            params![now(), code, used_by],
        )
        .map_err(|e| e.to_string())?;
    if claimed != 1 {
        return Err("票据已被使用".into());
    }
    tx.commit().map_err(|e| format!("提交兑换失败: {e}"))?;
    Ok(role)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 独占一个临时库 + 临时签名密钥(两者都是进程级环境变量,必须串行)。
    fn tmp_env() -> std::sync::MutexGuard<'static, ()> {
        let g = super::super::db::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let stamp = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        std::env::set_var(
            "POLARIS_COLLAB_DB",
            std::env::temp_dir().join(format!("authority-test-{stamp}.db")),
        );
        std::env::set_var(
            "POLARIS_ACCOUNT_KEY",
            std::env::temp_dir().join(format!("authority-test-{stamp}.key")),
        );
        std::env::remove_var("POLARIS_ACCOUNT_AUTHORITY");
        std::env::remove_var("POLARIS_ACCOUNT_AUTHORITY_URL");
        g
    }

    #[test]
    fn sign_then_verify_roundtrip() {
        let _g = tmp_env();
        let pk = public_key_b64().unwrap();
        let a = sign_assertion("acct_x", "alice", "a@b.com", "Alice").unwrap();
        let c = verify_assertion_with(&a, &pk).unwrap();
        assert_eq!(c.uid, "acct_x");
        assert_eq!(c.username, "alice");
        assert_eq!(c.email, "a@b.com");
    }

    #[test]
    fn tampered_assertion_is_rejected() {
        let _g = tmp_env();
        let pk = public_key_b64().unwrap();
        let a = sign_assertion("acct_x", "alice", "a@b.com", "Alice").unwrap();
        // 改载荷(把 alice 换成 root)后重新拼装 —— 签名覆盖载荷,必须验不过。
        let mut parts: Vec<&str> = a.split('.').collect();
        let raw = B64.decode(parts[1]).unwrap();
        let mut claims: Claims = serde_json::from_slice(&raw).unwrap();
        claims.username = "root".into();
        let forged = B64.encode(serde_json::to_vec(&claims).unwrap());
        parts[1] = &forged;
        let bad = parts.join(".");
        assert!(verify_assertion_with(&bad, &pk).is_err());
    }

    #[test]
    fn other_key_cannot_verify() {
        let _g = tmp_env();
        let a = sign_assertion("acct_x", "alice", "a@b.com", "Alice").unwrap();
        // 另一把钥匙的公钥:验不过(这就是「换把钥匙冒充权威」被挡住的那一刻)。
        let mut seed = [7u8; 32];
        seed[0] = 9;
        let other = SigningKey::from_bytes(&seed);
        let other_pub = B64.encode(other.verifying_key().to_bytes());
        assert!(verify_assertion_with(&a, &other_pub).is_err());
    }

    #[test]
    fn expired_assertion_is_rejected() {
        let _g = tmp_env();
        let pk = public_key_b64().unwrap();
        let sk = signing_key().unwrap();
        let claims = Claims {
            iss: kid_of(&pk),
            uid: "acct_x".into(),
            username: "alice".into(),
            email: "a@b.com".into(),
            display_name: "Alice".into(),
            iat: now() - 10_000,
            exp: now() - 9_000,
        };
        let head = format!("PA1.{}", B64.encode(serde_json::to_vec(&claims).unwrap()));
        let sig = sk.sign(head.as_bytes());
        let a = format!("{head}.{}", B64.encode(sig.to_bytes()));
        let e = verify_assertion_with(&a, &pk).unwrap_err();
        assert!(e.contains("过期"), "err={e}");
    }

    fn claims_of(uid: &str, username: &str, email: &str) -> Claims {
        Claims {
            iss: "k".into(),
            uid: uid.into(),
            username: username.into(),
            email: email.into(),
            display_name: username.into(),
            iat: now(),
            exp: now() + 300,
        }
    }

    #[test]
    fn first_member_becomes_owner_and_is_idempotent() {
        let _g = tmp_env();
        let c1 = claims_of("acct_1", "alice", "a@b.com");
        let u1 = upsert_member(&c1, Admission::ExistingOnly).unwrap();
        assert_eq!(u1.role, "owner", "空库第一个联邦账号应成为本机 owner");

        // 幂等:同 uid 再来一次不新建、不降级。
        let again = upsert_member(&c1, Admission::ExistingOnly).unwrap();
        assert_eq!(again.id, u1.id);
        assert_eq!(again.role, "owner");
    }

    /// 这次改动的安全线:**云端有账号 ≠ 本机欢迎你**。
    /// 主机已有主人之后,陌生的云端账号不带邀请码一律进不来 —— 否则谁在云端注册
    /// 一个号就能登进别人家的主机,而主机上的成员能触发构建脚本 = 主机 RCE。
    #[test]
    fn stranger_with_valid_identity_is_not_auto_admitted() {
        let _g = tmp_env();
        upsert_member(&claims_of("acct_1", "alice", "a@b.com"), Admission::ExistingOnly).unwrap();

        let stranger = claims_of("acct_2", "mallory", "m@b.com");
        let e = upsert_member(&stranger, Admission::ExistingOnly).unwrap_err();
        assert!(e.contains("邀请"), "err={e}");

        // 带票据角色才进得来,且角色由票据说了算(不是自封)。
        let u = upsert_member(&stranger, Admission::Invited("visitor".into())).unwrap();
        assert_eq!(u.role, "visitor");
        // 进来之后就是成员,日常登录不再需要票据。
        assert_eq!(
            upsert_member(&stranger, Admission::ExistingOnly).unwrap().role,
            "visitor"
        );
    }

    /// 一张票据只能用一次:并发/重复入伙不能各拿一个成员资格。
    #[test]
    fn ticket_is_burned_exactly_once() {
        let _g = tmp_env();
        upsert_member(&claims_of("acct_1", "alice", "a@b.com"), Admission::ExistingOnly).unwrap();
        let t = super::super::identity::create_ticket("collaborator", "t").unwrap();

        assert_eq!(burn_ticket(&t.code, "bob").unwrap(), "collaborator");
        let e = burn_ticket(&t.code, "carol").unwrap_err();
        assert!(e.contains("已被使用"), "err={e}");
    }

    #[test]
    fn federated_user_cannot_be_password_logged_in() {
        let _g = tmp_env();
        let c = Claims {
            iss: "k".into(),
            uid: "acct_1".into(),
            username: "alice".into(),
            email: "a@b.com".into(),
            display_name: "Alice".into(),
            iat: now(),
            exp: now() + 300,
        };
        upsert_member(&c, Admission::ExistingOnly).unwrap();
        // 哨兵不是合法 PHC —— 任何密码都不该进得去。
        assert!(super::super::auth::login("alice", "whatever", "d1").is_err());
        assert!(super::super::auth::login("alice", FEDERATED_SENTINEL, "d1").is_err());
    }

    /// 本次改动要证明的那件事:**一个账号,在两台互不相识的主机上都能登进去**。
    ///
    /// 两台主机 = 两个 collab.db(切 `POLARIS_COLLAB_DB` 就是换一台机器的视角:同一份代码、
    /// 另一份数据)。权威机建号签断言,两台成员主机各自**离线验签**、各自落自己的成员资格 ——
    /// 全程没有任何一台成员主机见过密码,也没有任何一台需要连得上权威。
    #[test]
    fn one_account_logs_into_two_independent_hosts() {
        let _g = tmp_env();
        let stamp = std::process::id();
        let db_authority = std::env::temp_dir().join(format!("auth-cloud-{stamp}.db"));
        let db_host_a = std::env::temp_dir().join(format!("auth-hostA-{stamp}.db"));
        let db_host_b = std::env::temp_dir().join(format!("auth-hostB-{stamp}.db"));
        for p in [&db_authority, &db_host_a, &db_host_b] {
            let _ = std::fs::remove_file(p);
        }

        // ── 云机(账号权威):建全局账号 + 签断言 ──
        std::env::set_var("POLARIS_COLLAB_DB", &db_authority);
        let pubkey = public_key_b64().unwrap();
        let (acct, uid) = super::super::auth::create_authority_account(
            "wuli",
            "s3cret-8",
            "武力",
            "wuli@example.com",
        )
        .unwrap();
        assert!(!uid.is_empty());
        // 权威侧用**邮箱**也登得进(注册绑的就是它)。
        let (_, uid2, email) =
            super::super::auth::authority_login("wuli@example.com", "s3cret-8").unwrap();
        assert_eq!(uid2, uid, "邮箱登录与用户名登录必须是同一个全局身份");
        assert_eq!(email, "wuli@example.com");
        let assertion = sign_assertion(&uid, &acct.username, &email, "武力").unwrap();

        // ── 主机 A(比如家里的桌面):空库,离线验签即进,首个成员当 owner ──
        std::env::set_var("POLARIS_COLLAB_DB", &db_host_a);
        let claims_a = verify_assertion_with(&assertion, &pubkey).unwrap();
        let ua = upsert_member(&claims_a, Admission::ExistingOnly).unwrap();
        assert_eq!(ua.username, "wuli");
        assert_eq!(ua.role, "owner");
        let token_a = super::super::auth::issue_session(ua.id, "devA").unwrap();
        assert_eq!(
            super::super::auth::check_session(&token_a).unwrap().username,
            "wuli"
        );

        // ── 主机 B(比如 NAS):另一座库,同一张断言照样进 ──
        std::env::set_var("POLARIS_COLLAB_DB", &db_host_b);
        let claims_b = verify_assertion_with(&assertion, &pubkey).unwrap();
        let ub = upsert_member(&claims_b, Admission::ExistingOnly).unwrap();
        assert_eq!(ub.username, "wuli");
        let token_b = super::super::auth::issue_session(ub.id, "devB").unwrap();

        // 全局身份同一个;两台机器上的本地行号与会话各自独立(互不影响,互不泄露)。
        assert_eq!(claims_a.uid, claims_b.uid, "跨主机认的必须是同一个 uid");
        assert!(
            super::super::auth::check_session(&token_a).is_err(),
            "主机 A 的会话不该在主机 B 上有效"
        );
        assert_eq!(
            super::super::auth::check_session(&token_b).unwrap().username,
            "wuli"
        );

        // 密码从没离开过权威:成员主机上拿真密码也登不进去。
        assert!(
            super::super::auth::login("wuli", "s3cret-8", "devB").is_err(),
            "联邦账号不该能在成员主机上用密码登录"
        );

        for p in [&db_authority, &db_host_a, &db_host_b] {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn username_squatting_is_refused() {
        let _g = tmp_env();
        // 本机先有一个同名的本地账号(uid 为空)。
        super::super::auth::create_user("alice", "s3cret-8", "owner", "Local Alice").unwrap();
        let c = Claims {
            iss: "k".into(),
            uid: "acct_1".into(),
            username: "alice".into(),
            email: "a@b.com".into(),
            display_name: "Alice".into(),
            iat: now(),
            exp: now() + 300,
        };
        let e = upsert_member(&c, Admission::ExistingOnly).unwrap_err();
        assert!(e.contains("同名"), "err={e}");
    }
}
