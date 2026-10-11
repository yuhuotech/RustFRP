//! 文件系统与环境变量的可注入抽象（便于测试，也避免业务逻辑直接依赖全局状态）。

use std::collections::BTreeMap;
use std::io;

/// 读取配置文件所需的文件系统能力。
pub trait ConfigFs {
    /// 读取文件为 UTF-8 文本。
    fn read_to_string(&self, path: &str) -> io::Result<String>;
    /// 列出目录下的文件名（不含子目录）。
    fn list_files(&self, dir: &str) -> io::Result<Vec<String>>;
    /// 目录是否存在。
    fn dir_exists(&self, dir: &str) -> bool;
    /// 写文件（覆盖）。含密钥的文件应只允许当前用户读写。
    fn write_private(&self, path: &str, content: &str) -> io::Result<()>;
    /// 创建目录（含父目录）。
    fn create_dir_all(&self, dir: &str) -> io::Result<()>;
}

/// 环境变量来源。
pub trait Env {
    /// 全部环境变量（模板 `.Envs` 使用）。
    fn vars(&self) -> BTreeMap<String, String>;
    /// 读取单个变量。
    fn get(&self, key: &str) -> Option<String> {
        self.vars().get(key).cloned()
    }
}

/// 真实文件系统。
#[derive(Debug, Default, Clone, Copy)]
pub struct StdFs;

impl ConfigFs for StdFs {
    fn read_to_string(&self, path: &str) -> io::Result<String> {
        std::fs::read_to_string(path)
    }

    fn list_files(&self, dir: &str) -> io::Result<Vec<String>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                out.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        out.sort();
        Ok(out)
    }

    fn dir_exists(&self, dir: &str) -> bool {
        std::path::Path::new(dir).is_dir()
    }

    fn write_private(&self, path: &str, content: &str) -> io::Result<()> {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        opts.open(path)?.write_all(content.as_bytes())
    }

    fn create_dir_all(&self, dir: &str) -> io::Result<()> {
        std::fs::create_dir_all(dir)
    }
}

/// 真实进程环境。
#[derive(Debug, Default, Clone, Copy)]
pub struct StdEnv;

impl Env for StdEnv {
    fn vars(&self) -> BTreeMap<String, String> {
        std::env::vars().collect()
    }
}

/// 内存环境（测试与导入场景使用）。
#[derive(Debug, Default, Clone)]
pub struct MapEnv(pub BTreeMap<String, String>);

impl Env for MapEnv {
    fn vars(&self) -> BTreeMap<String, String> {
        self.0.clone()
    }
}

/// 内存文件系统（测试与「面板预览导入」场景使用）。克隆共享同一份内容。
#[derive(Debug, Default, Clone)]
pub struct MemFs {
    files: std::sync::Arc<std::sync::Mutex<BTreeMap<String, String>>>,
}

impl MemFs {
    /// 新建空文件系统。
    pub fn new() -> Self {
        Self::default()
    }

    /// 写入一个文件，返回自身以便链式调用。
    pub fn with(self, path: &str, content: &str) -> Self {
        self.map().insert(path.to_string(), content.to_string());
        self
    }

    fn map(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, String>> {
        self.files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn split_dir(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(i) => (path.get(..i).unwrap_or(""), path.get(i + 1..).unwrap_or("")),
        None => (".", path),
    }
}

impl ConfigFs for MemFs {
    fn read_to_string(&self, path: &str) -> io::Result<String> {
        self.map()
            .get(path)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{path}: no such file")))
    }

    fn list_files(&self, dir: &str) -> io::Result<Vec<String>> {
        if !self.dir_exists(dir) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{dir}: no such directory"),
            ));
        }
        Ok(self
            .map()
            .keys()
            .filter_map(|p| {
                let (d, f) = split_dir(p);
                (d == dir).then(|| f.to_string())
            })
            .collect())
    }

    fn dir_exists(&self, dir: &str) -> bool {
        self.map().keys().any(|p| split_dir(p).0 == dir)
    }

    fn write_private(&self, path: &str, content: &str) -> io::Result<()> {
        self.map().insert(path.to_string(), content.to_string());
        Ok(())
    }

    fn create_dir_all(&self, _dir: &str) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn mem_fs_read_and_list() {
        let fs = MemFs::new()
            .with("a/x.toml", "1")
            .with("a/y.toml", "2")
            .with("top.toml", "3");
        assert_eq!(fs.read_to_string("a/x.toml").unwrap(), "1");
        assert_eq!(
            fs.read_to_string("nope").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(fs.list_files("a").unwrap(), vec!["x.toml", "y.toml"]);
        assert_eq!(fs.list_files(".").unwrap(), vec!["top.toml"]);
        assert_eq!(
            fs.list_files("missing").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert!(fs.dir_exists("a") && fs.dir_exists(".") && !fs.dir_exists("zzz"));
        fs.create_dir_all("new").unwrap();
        fs.write_private("new/f", "x").unwrap();
        assert_eq!(fs.read_to_string("new/f").unwrap(), "x");
        assert_eq!(
            fs.clone().read_to_string("new/f").unwrap(),
            "x",
            "clones share content"
        );
    }

    #[test]
    fn map_env() {
        let mut m = BTreeMap::new();
        m.insert("A".to_string(), "1".to_string());
        let env = MapEnv(m);
        assert_eq!(env.get("A").as_deref(), Some("1"));
        assert_eq!(env.get("B"), None);
        assert_eq!(env.vars().len(), 1);
    }

    #[test]
    fn std_env_sees_process_vars() {
        assert!(StdEnv.vars().contains_key("PATH"));
        assert!(StdEnv.get("PATH").is_some());
    }

    #[test]
    fn std_fs_roundtrip() {
        let dir = std::env::temp_dir().join(format!("rustfrp-envtest-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("b.toml"), "hello").unwrap();
        std::fs::write(dir.join("a.toml"), "x").unwrap();
        let d = dir.to_str().unwrap();
        assert_eq!(
            StdFs.read_to_string(&format!("{d}/b.toml")).unwrap(),
            "hello"
        );
        assert_eq!(StdFs.list_files(d).unwrap(), vec!["a.toml", "b.toml"]);
        assert!(StdFs.dir_exists(d));
        assert!(!StdFs.dir_exists(&format!("{d}/b.toml")));
        assert!(StdFs.list_files(&format!("{d}/none")).is_err());
        StdFs.create_dir_all(&format!("{d}/a/b")).unwrap();
        StdFs
            .write_private(&format!("{d}/a/b/secret"), "one")
            .unwrap();
        StdFs
            .write_private(&format!("{d}/a/b/secret"), "two")
            .unwrap();
        assert_eq!(
            StdFs.read_to_string(&format!("{d}/a/b/secret")).unwrap(),
            "two",
            "overwrites"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(format!("{d}/a/b/secret"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o077,
                0,
                "private files are not group/world accessible"
            );
        }
        assert!(StdFs
            .write_private(&format!("{d}/missing-dir/x"), "x")
            .is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
