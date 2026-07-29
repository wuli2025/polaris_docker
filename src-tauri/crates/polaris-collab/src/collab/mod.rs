//! 多人协作模块族（v8 方案第六节）。
//!
//! 铁律:主 Agent 裁决内容,永不裁决权限——权限判断全部走 db.rs 的确定性授权表。
pub mod account_store;
// 应用数据面 apihub 与桌面内嵌主机 hosting 是**壳件**(分发/拼装层, 认识全部引擎),
// 抽 crate 时归位壳仓 src/apihub.rs 与 src/hosting.rs —— 本 crate 只装协作 domain。
pub mod auth;
/// 账号权威:云机签发身份断言,成员主机离线验签(「一个账号在哪台机器都能登」的地基)。
/// 不挂任何 feature —— 桌面/NAS/云机三种形态都要么签要么验,谁都绕不开。
pub mod authority;
/// 任务级多轮对话(协作者↔负责人↔主 Agent 的微调通道)。
pub mod chat;
/// 任务卡检查工作流(CI-lite):worktree 跑开源工具链+密钥扫描+大文件闸。
pub mod checks;
#[cfg(feature = "desktop")]
pub mod commands;
pub mod db;
pub mod gitea;
/// 路径关押的远程文件浏览(fsface):隧道另一端的「盘」。双壳共用,仅需 collab-host。
#[cfg(feature = "collab-host")]
pub mod fsface;
/// 协作 HTTP 路由(axum,双壳共用):server 壳 merge 它;桌面 hosting 内嵌它。
#[cfg(feature = "collab-host")]
pub mod http;
pub mod identity;
/// 同账号设备网的设备目录(「登录即成网」)。权威侧的表与判定;不挂 feature ——
/// 云机要发号,桌面/NAS 侧的客户端循环也要复用这里的类型。
pub mod mesh;
/// 邮箱验证码(注册/找回密码)与 SMTP 发信。随协作路由走,仅 collab-host。
#[cfg(feature = "collab-host")]
pub mod mail;
pub mod lead;
pub mod lead_ai;
pub mod mergectl;
pub mod projects;
pub mod tasks;
pub mod teams;
pub mod workset;
// iroh 组网隧道:依赖树大,只随 collab-net feature 编译。
/// 云机中继网关:注册桌面主机 + 单 Endpoint 多主机 iroh 客户端 + /h/:id HTTP 反代。
/// 需 iroh(collab-net)+ axum(collab-host);云端 `--features server,collab-net` 构建。
#[cfg(all(feature = "collab-net", feature = "collab-host"))]
pub mod gateway;
#[cfg(feature = "collab-net")]
pub mod tunnel;
