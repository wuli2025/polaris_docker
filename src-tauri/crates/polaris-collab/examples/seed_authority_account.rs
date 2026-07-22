//! 给**账号权威库**播一个全局账号(运维/联调用)。
//!
//! 正常注册必须走「邮箱验证码 → /api/account/signup」,那条路要真发信。本工具是给
//! 没有 SMTP 的场景准备的旁路:云机首次上线要有第一个账号、联调要造测试账号时,
//! 直接往权威库里写。**它只能在能读写权威库文件的机器上跑** —— 不是网络接口,
//! 不构成「跳过邮箱验证」的远程口子。
//!
//! 用法:
//!   POLARIS_COLLAB_DB=/path/collab.db \
//!   cargo run -p polaris-collab --example seed_authority_account -- <用户名> <密码> <邮箱> [昵称]

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        eprintln!(
            "用法: seed_authority_account <用户名> <密码> <邮箱> [昵称]\n\
             (库位置由 POLARIS_COLLAB_DB 指定,默认 ~/Polaris/data/collab.db)"
        );
        std::process::exit(2);
    }
    let (username, password, email) = (&args[0], &args[1], &args[2]);
    let display = args.get(3).cloned().unwrap_or_else(|| username.clone());

    match polaris_collab::collab::auth::create_authority_account(
        username, password, &display, email,
    ) {
        Ok((user, uid)) => {
            println!("已建全局账号:");
            println!("  用户名   {}", user.username);
            println!("  邮箱     {email}");
            println!("  全局 uid {uid}");
        }
        Err(e) => {
            eprintln!("建账号失败: {e}");
            std::process::exit(1);
        }
    }
}
