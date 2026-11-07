//! MCP 的不覆盖文件操作，使用现有 SFTP 原语及有界树枚举。
use crate::sftp::{FileType, SftpOps, WalkResult};
use crate::{filter::ExcludeFilter, Error};
use sha2::{Digest, Sha256};

/// 规范父目录但保留末段，保证对软链接自身进行重命名或删除。
pub async fn destination(ops: &dyn SftpOps, raw: &str) -> Result<String, Error> {
    let raw = raw.trim_end_matches('/');
    let (parent, leaf) = raw.rsplit_once('/').unwrap_or((".", raw));
    if leaf.is_empty() || leaf == "." || leaf == ".." || leaf.contains('\0') {
        return Err(Error::Sftp("ambiguous or root path is not allowed".into()));
    }
    let parent = ops
        .canonicalize(if parent.is_empty() { "/" } else { parent })
        .await?;
    Ok(format!("{}/{leaf}", parent.trim_end_matches('/')))
}

pub async fn walk(
    ops: &dyn SftpOps,
    path: &str,
    exclude: Option<&str>,
) -> Result<WalkResult, Error> {
    if ops.lstat(path).await?.file_type != FileType::Dir {
        return Err(Error::Sftp(
            "walk root must be a directory, without following links".into(),
        ));
    }
    crate::sftp::walk_files_with_budget(
        ops,
        path,
        &ExcludeFilter::parse(exclude.unwrap_or("")),
        128 * 1024,
    )
    .await
}

pub async fn require_absent(ops: &dyn SftpOps, path: &str) -> Result<(), Error> {
    match ops.lstat(path).await {
        Ok(_) => Err(Error::Conflict(path.into())),
        Err(Error::SftpStatus { code: 2, .. }) => Ok(()),
        Err(error) => Err(error),
    }
}

/// 每次操作前核对父路径仍是预检所得目录，拒绝跟随后来替换的软链。
async fn unchanged_parent(ops: &dyn SftpOps, path: &str) -> Result<(), Error> {
    if destination(ops, path).await? != path {
        return Err(Error::Sftp("directory changed after preflight".into()));
    }
    Ok(())
}

async fn preflight(ops: &dyn SftpOps, root: &str, tree: &WalkResult) -> Result<(), Error> {
    for dir in &tree.dirs {
        let path = format!("{root}/{dir}");
        unchanged_parent(ops, &path).await?;
        if ops.lstat(&path).await?.file_type != FileType::Dir {
            return Err(Error::Sftp("directory changed after enumeration".into()));
        }
    }
    for file in &tree.files {
        let path = format!("{root}/{file}");
        unchanged_parent(ops, &path).await?;
        if ops.lstat(&path).await?.file_type != FileType::Regular {
            return Err(Error::Sftp(
                "tree contains changed or unsupported files".into(),
            ));
        }
    }
    Ok(())
}

pub async fn copy_file(ops: &dyn SftpOps, from: &str, to: &str) -> Result<(), Error> {
    unchanged_parent(ops, from).await?;
    unchanged_parent(ops, to).await?;
    let before = ops.lstat(from).await?;
    if before.file_type != FileType::Regular {
        return Err(Error::Sftp("copy source must be a regular file".into()));
    }
    let mut reader = ops.open_reader(from).await?;
    let mut writer = ops.create_exclusive(to).await?;
    let mut hash = Sha256::new();
    let mut offset = 0;
    while offset < before.size {
        let bytes = reader
            .read_at(
                offset,
                (before.size - offset).min(crate::transfer::CHUNK as u64) as usize,
            )
            .await?;
        if bytes.is_empty() {
            return Err(Error::Sftp("source became shorter during copy".into()));
        }
        hash.update(&bytes);
        writer.write(&bytes).await?;
        offset += bytes.len() as u64;
    }
    writer.finish().await?;
    ops.sync(to).await?;
    let after = ops.lstat(from).await?;
    if after.size != before.size
        || after.mtime != before.mtime
        || after.file_type != before.file_type
    {
        return Err(Error::Sftp("source changed during copy".into()));
    }
    let mut verify = ops.open_reader(to).await?;
    let mut actual = Sha256::new();
    let mut offset = 0;
    while offset < before.size {
        let bytes = verify
            .read_at(
                offset,
                (before.size - offset).min(crate::transfer::CHUNK as u64) as usize,
            )
            .await?;
        if bytes.is_empty() {
            return Err(Error::Sftp("copied data is incomplete".into()));
        }
        actual.update(&bytes);
        offset += bytes.len() as u64;
    }
    if ops.stat_size(to).await? != before.size || hash.finalize() != actual.finalize() {
        return Err(Error::Sftp(
            "copied data failed SHA-256 verification".into(),
        ));
    }
    Ok(())
}

pub async fn copy(
    ops: &dyn SftpOps,
    from: &str,
    to: &str,
) -> Result<serde_json::Value, (Error, serde_json::Value)> {
    let mut completed = 0usize;
    let mut staging = None;
    let result: Result<serde_json::Value, Error> = async {
        let from = destination(ops, from).await?;
        let to = destination(ops, to).await?;
        if to == from || to.starts_with(&format!("{}/", from.trim_end_matches('/'))) {
            return Err(Error::Sftp("cannot copy a directory into itself".into()));
        }
        let meta = ops.lstat(&from).await?;
        let tree = match meta.file_type {
            FileType::Dir => Some(walk(ops, &from, None).await?),
            FileType::Regular => None,
            _ => return Err(Error::Sftp("unsupported copy source".into())),
        };
        if tree.as_ref().is_some_and(|t| !t.skipped_links.is_empty()) {
            return Err(Error::Sftp(
                "copy contains soft links; no files were copied".into(),
            ));
        }
        require_absent(ops, &to).await?;
        if let Some(tree) = &tree {
            preflight(ops, &from, tree).await?;
        }
        let tmp = format!("{to}.fs-copy-{}", uuid::Uuid::new_v4());
        staging = Some(tmp.clone());
        if let Some(tree) = tree {
            ops.mkdir(&tmp).await?;
            let mut dirs = tree.dirs;
            dirs.sort_by_key(|p| p.split('/').count());
            for dir in dirs {
                let path = format!("{tmp}/{dir}");
                unchanged_parent(ops, &path).await?;
                ops.mkdir(&path).await?;
            }
            for file in tree.files {
                copy_file(ops, &format!("{from}/{file}"), &format!("{tmp}/{file}")).await?;
                completed += 1;
            }
        } else {
            copy_file(ops, &from, &tmp).await?;
            completed += 1;
        }
        unchanged_parent(ops, &tmp).await?;
        unchanged_parent(ops, &to).await?;
        ops.rename_no_replace(&tmp, &to).await?;
        staging = None;
        Ok(serde_json::json!({"copied_files":completed,"path":to}))
    }
    .await;
    result.map_err(|error| {
        (
            error,
            serde_json::json!({"copied_files":completed,"staging_path":staging}),
        )
    })
}

/// 预检完整树后再执行，不跟随软链，异常带回已完成计数。
pub async fn delete(
    ops: &dyn SftpOps,
    raw: &str,
    recursive: bool,
) -> Result<serde_json::Value, (Error, serde_json::Value)> {
    let mut removed = 0usize;
    let mut current = raw.to_string();
    let result: Result<serde_json::Value, Error> = async {
        let path = destination(ops, raw).await?;
        let meta = ops.lstat(&path).await?;
        if meta.file_type != FileType::Dir {
            ops.remove(&path).await?;
            return Ok(serde_json::json!({"removed":1}));
        }
        if !recursive {
            ops.remove_dir(&path).await?;
            return Ok(serde_json::json!({"removed":1}));
        }
        let tree = walk(ops, &path, None).await?;
        preflight(ops, &path, &tree).await?;
        for file in tree.files.into_iter().chain(tree.skipped_links) {
            current = format!("{path}/{file}");
            unchanged_parent(ops, &current).await?;
            if ops.lstat(&current).await?.file_type == FileType::Dir {
                return Err(Error::Sftp(
                    "file became a directory during deletion".into(),
                ));
            }
            ops.remove(&current).await?;
            removed += 1;
        }
        let mut dirs = tree.dirs;
        dirs.sort_by_key(|p| std::cmp::Reverse(p.split('/').count()));
        for dir in dirs {
            current = format!("{path}/{dir}");
            unchanged_parent(ops, &current).await?;
            ops.remove_dir(&current).await?;
            removed += 1;
        }
        current = path;
        unchanged_parent(ops, &current).await?;
        ops.remove_dir(&current).await?;
        removed += 1;
        Ok(serde_json::json!({"removed":removed}))
    }
    .await;
    result.map_err(|error| {
        (
            error,
            serde_json::json!({"removed":removed,"failed_path":current}),
        )
    })
}
