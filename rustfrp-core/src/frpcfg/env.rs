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

/// 内存文件系统（测试与「面板预览导入」场景使用）。
#[derive(Debug, Default, Clone)]
pub struct MemFs {
    files: BTreeMap<String, String>,
}

impl MemFs {
    /// 新建空文件系统。
    pub fn new() -> Self {
        Self::default()
    }

    /// 写入一个文件，返回自身以便链式调用。
    pub fn with(mut self, path: &str, content: &str) -> Self {
        self.files.insert(path.to_string(), content.to_string());
        self
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
        self.files
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
            .files
            .keys()
            .filter_map(|p| {
                let (d, f) = split_dir(p);
                (d == dir).then(|| f.to_string())
            })
            .collect())
    }

    fn dir_exists(&self, dir: &str) -> bool {
        self.files.keys().any(|p| split_dir(p).0 == dir)
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
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
