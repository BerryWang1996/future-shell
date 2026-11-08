//! 终端传输的固定命令与名称契约；上传须有 /bin/sh 与 rz。
//! 返回值不含提交键；最终 writer 在被动预约成功后统一追加实际 CR。
use fs_sshengine::{sandbox::escape_local_component, verify::shell_quote};
use std::path::{Component, Path};

type Result<T> = std::result::Result<T, &'static str>;

fn check_path(path: &str) -> Result<()> {
    // 引号保护 shell 语法，不能保护 PTY 行编辑器的 Tab/ESC/退格等按键。
    if path.is_empty() || path.chars().any(char::is_control) {
        return Err("路径不得为空或包含控制字符");
    }
    Ok(())
}

fn check_basename(name: &str) -> Result<()> {
    check_path(name)?;
    if matches!(name, "." | "..") || name.contains(['/', '\\']) || name.trim() != name {
        return Err("文件名不能经 ZMODEM 基名化或 trim 改变");
    }
    Ok(())
}

/// 新 terminal.upload 的相对源路径；授权根及稳定 FD 在调用层另行核验。
pub(crate) fn upload_basename(relative: &Path) -> Result<&str> {
    let path = relative.to_str().ok_or("上传路径必须是 UTF-8")?;
    check_path(path)?;
    if relative.is_absolute()
        || relative.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
        || path.ends_with(['/', '\\'])
        || matches!(
            path.rsplit(std::path::is_separator).next(),
            Some("." | "..")
        )
    {
        return Err("上传路径必须是授权根内的相对文件路径");
    }
    let name = relative
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("缺少上传文件名")?;
    check_basename(name)?;
    Ok(name)
}

pub(crate) fn upload_command(remote_dir: &str) -> Result<String> {
    check_path(remote_dir)?;
    // ./ 同时防 cd 的选项解释及 CDPATH；不展开 ~、$ 或 glob。
    // 固定 /bin/sh 子进程避开外层 zsh 的 command cd 差异及函数/alias；
    // 目录仅经位置参数传入，不能成为脚本语法，外层工作目录保持不变。
    let dir = if remote_dir.starts_with('/') {
        remote_dir.into()
    } else {
        format!("./{remote_dir}")
    };
    Ok(format!(
        "command /bin/sh -c {} fs-terminal-upload {}",
        shell_quote("command cd \"$1\" && command rz -b -e -p"),
        shell_quote(&dir)
    ))
}

/// 协议原名与可逆安全本地名分开交付，调用层须披露 local_relative_path。
pub(crate) struct DownloadCommand {
    pub command: String,
    pub basename: String,
    pub local_basename: String,
}

pub(crate) fn download_command(remote_path: &str) -> Result<DownloadCommand> {
    check_path(remote_path)?;
    let basename = remote_path.rsplit('/').next().ok_or("缺少下载文件名")?;
    check_basename(basename)?;
    let path = if remote_path.starts_with('-') {
        format!("./{remote_path}")
    } else {
        remote_path.into()
    };
    Ok(DownloadCommand {
        command: format!("command sz -b -e -- {}", shell_quote(&path)),
        basename: basename.into(),
        local_basename: escape_local_component(basename),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_protocol_name_changes_and_line_editor_controls() {
        for bad in [
            "", ".", "..", "/", "dir/", "dir/.", "dir/..", " x", "x ", "a\0b", "a\rb", "a\nb",
            "a\tb", "a\u{1b}b", "a\u{7f}b",
        ] {
            assert!(download_command(bad).is_err(), "{bad:?}");
            assert!(upload_basename(Path::new(bad)).is_err(), "{bad:?}");
        }
        // 远端路径按 POSIX 解释；本地 Path 使用宿主平台的分隔符语义。
        assert!(download_command("dir/a\\b").is_err());
        #[cfg(not(windows))]
        assert!(upload_basename(Path::new("dir/a\\b")).is_err());
        #[cfg(windows)]
        assert_eq!(upload_basename(Path::new("dir/a\\b")).unwrap(), "b");
        for bad in ["", "a\0b", "a\rb", "a\nb", "a\tb", "a\u{1b}b", "a\u{7f}b"] {
            assert!(upload_command(bad).is_err(), "{bad:?}");
        }
        for bad in ["/absolute/file", "../file", "nested/../file"] {
            assert!(upload_basename(Path::new(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn source_names_are_relative_and_preserved_and_download_encoding_is_explicit() {
        assert_eq!(
            upload_basename(Path::new("nested/中文 a'b.txt")).unwrap(),
            "中文 a'b.txt"
        );
        assert_eq!(
            upload_basename(Path::new("./-leading")).unwrap(),
            "-leading"
        );
        let download = download_command("dir/a:b%?.txt").unwrap();
        assert_eq!(download.basename, "a:b%?.txt");
        assert_eq!(
            fs_sshengine::sandbox::unescape_local_component(&download.local_basename).as_deref(),
            Some(download.basename.as_str())
        );
        assert_ne!(download.local_basename, download.basename);
        assert!(!download.command.contains(['\r', '\n']));
        assert!(!upload_command(" dir ").unwrap().contains(['\r', '\n']));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_non_utf8_source_names() {
        use std::os::unix::ffi::OsStringExt;
        let name = std::ffi::OsString::from_vec(vec![b'a', 0xff]);
        assert!(upload_basename(Path::new(&name)).is_err());
    }

    #[cfg(unix)]
    fn shell_stub(shell: &str, command: &str, root: &Path) -> (Vec<String>, String) {
        use std::os::unix::fs::PermissionsExt;
        let bin = root.join("stub-bin");
        std::fs::create_dir_all(&bin).unwrap();
        for name in ["rz", "sz"] {
            let stub = bin.join(name);
            std::fs::write(
                &stub,
                "#!/bin/sh\nprintf '%s\\0' \"$PWD\" \"$@\" > \"$FS_ARGV\"\n",
            )
            .unwrap();
            std::fs::set_permissions(stub, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let argv = root.join("argv");
        let after = root.join("after");
        let output = std::process::Command::new(shell)
            .args(if shell.ends_with("zsh") { vec!["-f"] } else { vec![] })
            .arg("-c")
            .arg(format!(
                "cd() {{ printf HOOK > \"$FS_CD_HOOK\"; }}\nalias cd=false\n{command}\nresult=$?\nprintf '%s' \"$PWD\" > \"$FS_AFTER\"\nexit \"$result\""
            ))
            .current_dir(root)
            .env("PATH", std::env::join_paths([bin.as_path(), Path::new("/usr/bin"), Path::new("/bin")]).unwrap())
            .env("FS_ARGV", &argv)
            .env("FS_AFTER", &after)
            .env("CDPATH", root.join("cdpath-decoy"))
            .env("FS_CD_HOOK", root.join("cd-hook"))
            .output()
            .unwrap();
        assert!(
            !root.join("cd-hook").exists(),
            "command invoked shell cd function"
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let fields = std::fs::read(&argv)
            .unwrap()
            .split(|b| *b == 0)
            .filter(|b| !b.is_empty())
            .map(|b| String::from_utf8(b.to_vec()).unwrap())
            .collect();
        (fields, std::fs::read_to_string(after).unwrap())
    }

    #[cfg(unix)]
    #[test]
    fn real_shells_preserve_target_argv_outer_cwd_and_literal_metacharacters() {
        for shell in ["/bin/sh", "/bin/bash", "/bin/zsh"] {
            if !Path::new(shell).is_file() {
                continue;
            }
            eprintln!("real shell oracle: {shell}");
            let scratch = tempfile::tempdir().unwrap();
            let root = scratch.path().canonicalize().unwrap();
            let names = [
                " dir with spaces ",
                "中文",
                "a'b",
                "-leading",
                "~",
                "$HOME",
                "[ab]*",
                "$(printf HACK > pwned)",
                "`printf HACK > pwned`",
            ];
            for name in names {
                let dir = root.join(name);
                std::fs::create_dir(&dir).unwrap();
                std::fs::create_dir_all(root.join("cdpath-decoy").join(name)).unwrap();
                let (argv, after) = shell_stub(shell, &upload_command(name).unwrap(), &root);
                assert_eq!(argv, [dir.to_str().unwrap(), "-b", "-e", "-p"]);
                assert_eq!(after, root.to_str().unwrap());
                assert!(!root.join("pwned").exists(), "upload executed {name:?}");
                let remote = format!("{name}/file.txt");
                let plan = download_command(&remote).unwrap();
                let (argv, after) = shell_stub(shell, &plan.command, &root);
                let expected = if remote.starts_with('-') {
                    format!("./{remote}")
                } else {
                    remote
                };
                assert_eq!(argv, [root.to_str().unwrap(), "-b", "-e", "--", &expected]);
                assert_eq!(after, root.to_str().unwrap());
                assert!(!root.join("pwned").exists(), "download executed {name:?}");
            }
            let absolute = root.join("中文");
            let (argv, after) = shell_stub(
                shell,
                &upload_command(absolute.to_str().unwrap()).unwrap(),
                &root,
            );
            assert_eq!(argv[0], absolute.to_str().unwrap());
            assert_eq!(after, root.to_str().unwrap());
        }
    }
}
