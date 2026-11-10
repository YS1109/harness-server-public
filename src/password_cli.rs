//! Offline interactive password hashing. Passwords never enter command arguments or config.
use crate::error::{Result, invalid};
use std::io::IsTerminal;

pub fn run() -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return Err(invalid(
            "请在交互终端运行hash-password；密码使用隐藏输入，不接受命令参数或管道",
        ));
    }
    let password = rpassword::prompt_password("管理员密码（隐藏输入）：")?;
    let repeated = rpassword::prompt_password("再次输入密码：")?;
    if password != repeated {
        return Err(invalid("两次输入的密码不一致"));
    }
    let hash = crate::admin_session::hash_password(&password)?;
    println!("{hash}");
    Ok(())
}
