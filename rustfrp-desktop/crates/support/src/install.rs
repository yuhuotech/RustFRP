//! Package strategies preserve the existing release names and retain rollback copies until restart succeeds.
use crate::{process, startup::Platform, Error};
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

#[derive(Clone, Debug)]
pub enum Method {
    Mac { bundle: PathBuf },
    Tar { root: PathBuf, relative: PathBuf },
    Deb { executable: PathBuf },
    Windows { executable: PathBuf },
    Manual,
}
#[derive(Clone, Debug)]
pub struct Plan {
    pub asset: Option<String>,
    pub manual: Option<String>,
    pub method: Method,
}
pub fn plan(
    platform: Platform,
    arch: &str,
    executable: &Path,
    installed: bool,
    assets: &[String],
    writable: impl Fn(&Path) -> bool,
) -> Plan {
    let manual = |message: &str| Plan {
        asset: None,
        manual: Some(message.into()),
        method: Method::Manual,
    };
    if !executable.is_absolute() || executable.to_string_lossy().chars().any(char::is_control) {
        return manual("无法识别当前安装位置，请前往下载页手动更新");
    }
    let available = |name: &str| assets.iter().any(|asset| asset == name);
    match platform {
        Platform::Mac => {
            let arch = match arch {
                "aarch64" | "arm64" => "arm64",
                "x86_64" => "x86_64",
                _ => return manual("此版本没有本机架构的 macOS 更新包"),
            };
            let asset = format!("rustfrp-macos-{arch}.zip");
            if !available(&asset) {
                return manual("此版本没有本机架构的 macOS 更新包");
            }
            let Some(bundle) = executable
                .ancestors()
                .find(|path| path.extension().is_some_and(|extension| extension == "app"))
            else {
                return manual("无法定位 RustFRP.app，请手动更新");
            };
            if bundle.starts_with("/Volumes")
                || bundle.to_string_lossy().contains("/AppTranslocation/")
            {
                return manual("请先把应用移到应用程序文件夹再更新");
            }
            if !bundle.parent().is_some_and(writable) {
                return manual("应用程序目录不可写，请手动安装更新");
            }
            Plan {
                asset: Some(asset),
                manual: None,
                method: Method::Mac {
                    bundle: bundle.into(),
                },
            }
        }
        Platform::Windows => {
            let asset = "rustfrp-windows-x86_64-setup.exe";
            if arch != "x86_64" || !available(asset) {
                return manual("此版本没有 Windows x64 安装程序");
            }
            if !installed {
                return manual("当前是免安装目录，自动更新仅支持安装程序版本");
            }
            Plan {
                asset: Some(asset.into()),
                manual: None,
                method: Method::Windows {
                    executable: executable.into(),
                },
            }
        }
        Platform::Linux => {
            if arch != "x86_64" {
                return manual("当前只发布 Linux x86_64 更新包");
            }
            if installed && available("rustfrp-linux-x86_64.deb") {
                return Plan {
                    asset: Some("rustfrp-linux-x86_64.deb".into()),
                    manual: None,
                    method: Method::Deb {
                        executable: executable.into(),
                    },
                };
            }
            let asset = "rustfrp-linux-x86_64.tar.gz";
            let Some(app) = executable.parent() else {
                return manual("安装目录不支持自动更新");
            };
            let Some(root) = app.parent() else {
                return manual("安装目录不支持自动更新");
            };
            if available(asset)
                && app.file_name().is_some_and(|name| name == "app")
                && root.parent().is_some_and(writable)
            {
                let Ok(relative) = executable.strip_prefix(root) else {
                    return manual("安装位置不兼容");
                };
                return Plan {
                    asset: Some(asset.into()),
                    manual: None,
                    method: Method::Tar {
                        root: root.into(),
                        relative: relative.into(),
                    },
                };
            }
            manual("此安装位置不支持自动替换，请下载安装包手动更新")
        }
    }
}
pub fn writable(path: &Path) -> bool {
    tempfile::NamedTempFile::new_in(path).is_ok()
}
fn installed(platform: Platform, executable: &Path) -> bool {
    if platform == Platform::Windows {
        executable.parent().is_some_and(|parent| {
            parent.join("unins000.exe").is_file() || parent.join("uninstall.exe").is_file()
        })
    } else {
        executable.starts_with("/usr/lib/rustfrp/app")
            || executable == Path::new("/usr/bin/rustfrp_app")
    }
}
pub fn current(executable: &Path, assets: &[String]) -> Plan {
    let platform = Platform::current();
    plan(
        platform,
        std::env::consts::ARCH,
        executable,
        installed(platform, executable),
        assets,
        writable,
    )
}
fn quote(value: &Path) -> String {
    format!("'{}'", value.to_string_lossy().replace('\'', "'\\''"))
}
fn wait_for_exit(pid: u32) -> String {
    format!("i=0\nwhile kill -0 {pid} 2>/dev/null && [ \"$i\" -lt 100 ]; do sleep 0.3; i=$((i+1)); done\nkill -0 {pid} 2>/dev/null && exit 1\n")
}

pub fn replace_script(
    target: &Path,
    fresh: &Path,
    executable: &Path,
    stage: &Path,
    download: &Path,
    mac: bool,
) -> String {
    let launch = if mac {
        format!("/usr/bin/open {}", quote(target))
    } else {
        format!("setsid {} >/dev/null 2>&1 &", quote(executable))
    };
    // pgrep -f receives a literal fixed expression only for a path created by our package strategy.
    let expression = executable
        .to_string_lossy()
        .chars()
        .flat_map(|character| {
            if matches!(
                character,
                '.' | '[' | ']' | '(' | ')' | '*' | '+' | '?' | '^' | '$' | '{' | '}' | '|' | '\\'
            ) {
                vec!['\\', character]
            } else {
                vec![character]
            }
        })
        .collect::<String>();
    let expression = format!("'{}'", expression.replace('\'', "'\\''"));
    format!("TARGET={}\nFRESH={}\nOLD=\"$TARGET.old.$$\"\nmv \"$TARGET\" \"$OLD\" || exit 1\nif mv \"$FRESH\" \"$TARGET\"; then\n  {launch}\n  sleep 3\n  if pgrep -f -- {expression} >/dev/null; then\n    rm -rf \"$OLD\" {} {}\n    exit 0\n  fi\n  rm -rf \"$TARGET\"\nfi\nmv \"$OLD\" \"$TARGET\"\nexit 1\n",quote(target),quote(fresh),quote(stage),quote(download))
}
fn spawn_script(script: &Path) -> Result<(), Error> {
    if !std::fs::symlink_metadata(script)
        .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
    {
        return Err(Error::invalid("安装接续文件不存在或类型无效"));
    }
    let mut command = Command::new("/bin/sh");
    command
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
        .spawn()
        .map_err(Error::io_context("无法启动更新安装器，原应用已保留"))?;
    Ok(())
}
pub trait Driver {
    fn run(
        &self,
        program: &str,
        args: &[String],
        deadline: Duration,
    ) -> Result<process::Output, Error>;
    fn shell(&self, path: &Path) -> Result<(), Error>;
    fn windows(&self, package: &Path, executable: &Path) -> Result<(), Error>;
}
struct Native;
impl Driver for Native {
    fn run(
        &self,
        program: &str,
        args: &[String],
        deadline: Duration,
    ) -> Result<process::Output, Error> {
        process::run(program, args, deadline)
    }
    fn shell(&self, path: &Path) -> Result<(), Error> {
        spawn_script(path)
    }
    fn windows(&self, package: &Path, executable: &Path) -> Result<(), Error> {
        #[cfg(not(windows))]
        {
            let _ = (package, executable);
            Err(Error::invalid("Windows 安装程序仅可在 Windows 上运行"))
        }
        #[cfg(windows)]
        {
            let download = package
                .parent()
                .ok_or_else(|| Error::invalid("更新包路径无效"))?;
            let script = download.join("install.ps1");
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&script)
                .map_err(Error::io_context("无法保存 Windows 安装接续文件"))?;
            file.write_all(b"param([string]$Package,[string]$Application,[int]$ParentPid)\n$ErrorActionPreference='Stop'\nfor($i=0;$i -lt 100 -and (Get-Process -Id $ParentPid -ErrorAction SilentlyContinue);$i++){Start-Sleep -Milliseconds 300}\nif(Get-Process -Id $ParentPid -ErrorAction SilentlyContinue){exit 1}\ntry{$directory=Split-Path -Parent $Application;$result=Start-Process -FilePath $Package -ArgumentList @('/S',\"/D=$directory\") -PassThru -Wait;if($result.ExitCode -eq 0){Start-Process -FilePath $Application};exit $result.ExitCode}finally{Remove-Item -LiteralPath (Split-Path -Parent $Package) -Recurse -Force}\n").and_then(|()|file.sync_all()).map_err(Error::io_context("无法保存 Windows 安装接续文件"))?;
            let mut command = Command::new("powershell.exe");
            command
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-File",
                ])
                .arg(&script)
                .arg("-Package")
                .arg(package)
                .arg("-Application")
                .arg(executable)
                .arg("-ParentPid")
                .arg(std::process::id().to_string())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x00000200 | 0x00000008);
            }
            command
                .spawn()
                .map_err(Error::io_context("无法启动 Windows 安装接续进程"))?;
            Ok(())
        }
    }
}
pub fn handoff(plan: &Plan, package: &Path) -> Result<(), Error> {
    handoff_with(plan, package, &Native)
}
pub fn handoff_with(plan: &Plan, package: &Path, driver: &impl Driver) -> Result<(), Error> {
    if !package.is_absolute()
        || !std::fs::symlink_metadata(package)
            .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
    {
        return Err(Error::invalid("更新包须为私有下载目录中的普通文件"));
    }
    let download = package
        .parent()
        .ok_or_else(|| Error::invalid("更新包路径无效"))?;
    match &plan.method {
        Method::Windows { executable } => driver.windows(package, executable),
        Method::Manual => Err(Error::invalid("此安装方式需手动更新")),
        method => {
            let (stage,body)=match method {
                Method::Mac {bundle}=>{
                    let parent=bundle.parent().ok_or_else(||Error::invalid("应用目录无效"))?;
                    let stage=tempfile::Builder::new().prefix(".rustfrp-update-").tempdir_in(parent).map_err(Error::io_context("无法创建私有更新目录"))?;
                    let output=driver.run("/usr/bin/ditto",&["-x".into(),"-k".into(),package.to_string_lossy().into_owned(),stage.path().to_string_lossy().into_owned()],Duration::from_secs(60))?;
                    if !output.success {return Err(Error::io("解压 macOS 更新包失败"));}
                    let bundles=std::fs::read_dir(stage.path()).map_err(Error::io_context("更新包目录不可读"))?.filter_map(Result::ok).filter(|entry| entry.file_type().is_ok_and(|kind|kind.is_dir())).map(|entry|entry.path()).filter(|path|path.extension().is_some_and(|extension|extension=="app")).collect::<Vec<_>>();
                    let fresh=bundles.first().filter(|_|bundles.len()==1).ok_or_else(||Error::invalid("更新包必须含一个 .app"))?;
                    let executable=bundle.join("Contents/MacOS/rustfrp_app");
                    if !std::fs::symlink_metadata(fresh.join("Contents/MacOS/rustfrp_app")).is_ok_and(|metadata|metadata.is_file() && !metadata.file_type().is_symlink()) {return Err(Error::invalid("更新包缺少应用可执行文件"));}
                    if !driver.run("/usr/bin/codesign",&["--verify".into(),"--deep".into(),"--strict".into(),fresh.to_string_lossy().into_owned()],Duration::from_secs(30))?.success {return Err(Error::invalid("应用代码签名验证失败，已拒绝替换"));}
                    let body=replace_script(bundle,fresh,&executable,stage.path(),download,true); (Some(stage),body)
                }
                Method::Tar {root,relative}=>{
                    if relative.as_os_str().is_empty() || relative.components().any(|part|!matches!(part,std::path::Component::Normal(_))) {return Err(Error::invalid("更新包可执行文件路径无效"));}
                    let parent=root.parent().ok_or_else(||Error::invalid("安装目录无效"))?;
                    let stage=tempfile::Builder::new().prefix(".rustfrp-update-").tempdir_in(parent).map_err(Error::io_context("无法创建私有更新目录"))?;
                    let output=driver.run("/bin/tar",&["-xzf".into(),package.to_string_lossy().into_owned(),"-C".into(),stage.path().to_string_lossy().into_owned()],Duration::from_secs(60))?;
                    let fresh=stage.path().join("rustfrp-linux-x86_64"); if !output.success || !std::fs::symlink_metadata(fresh.join(relative)).is_ok_and(|metadata|metadata.is_file() && !metadata.file_type().is_symlink()) {return Err(Error::invalid("Linux 更新包结构无效"));}
                    let body=replace_script(root,&fresh,&root.join(relative),stage.path(),download,false); (Some(stage),body)
                }
                Method::Deb {executable} => (None,format!("export DEBIAN_FRONTEND=noninteractive\nif [ \"$(id -u)\" = 0 ]; then dpkg -i {}\nelif sudo -n true 2>/dev/null; then sudo -n dpkg -i {}\nelse pkexec dpkg -i {}; fi\nRESULT=$?\nrm -rf {}\nif [ $RESULT -eq 0 ]; then setsid {} >/dev/null 2>&1 & fi\nexit $RESULT\n",quote(package),quote(package),quote(package),quote(download),quote(executable))),
                _=>return Err(Error::invalid("安装方式无效")),
            };
            let path = download.join("install.sh");
            let script = format!("#!/bin/sh\n{}{}", wait_for_exit(std::process::id()), body);
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(Error::io_context("无法创建安装接续文件"))?;
            file.write_all(script.as_bytes())
                .and_then(|()| file.sync_all())
                .map_err(Error::io_context("无法保存安装接续文件"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                    .map_err(Error::io_context("无法保护更新安装器"))?;
            }
            driver.shell(&path)?;
            if let Some(stage) = stage {
                let _ = stage.keep();
            }
            Ok(())
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Clone, Copy, Debug)]
    enum Shape {
        Mac,
        Tar,
        Empty,
        TwoApps,
        MissingExecutable,
        SymlinkExecutable,
        MissingDirectory,
        ExtractionFailure,
        ExtractionError,
        SignatureFailure,
        SignatureError,
    }
    struct Recorded {
        shape: Shape,
        fail_shell: bool,
        shells: Mutex<Vec<String>>,
        stages: Mutex<Vec<PathBuf>>,
        windows: Mutex<Vec<(PathBuf, PathBuf)>>,
    }
    impl Recorded {
        fn new(shape: Shape) -> Self {
            Self {
                shape,
                fail_shell: false,
                shells: Mutex::new(Vec::new()),
                stages: Mutex::new(Vec::new()),
                windows: Mutex::new(Vec::new()),
            }
        }
    }
    impl Driver for Recorded {
        fn run(
            &self,
            program: &str,
            args: &[String],
            _deadline: Duration,
        ) -> Result<process::Output, Error> {
            let signature = program.ends_with("codesign");
            if matches!(
                (signature, self.shape),
                (false, Shape::ExtractionError) | (true, Shape::SignatureError)
            ) {
                return Err(Error::io("system operation failed"));
            }
            if !signature {
                let stage = PathBuf::from(args.last().unwrap());
                self.stages.lock().unwrap().push(stage.clone());
                let make = |path: &Path| {
                    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                    std::fs::write(path, b"executable").unwrap();
                };
                match self.shape {
                    Shape::Mac | Shape::SignatureError | Shape::SignatureFailure => {
                        make(&stage.join("RustFRP.app/Contents/MacOS/rustfrp_app"))
                    }
                    Shape::Tar => make(&stage.join("rustfrp-linux-x86_64/app/rustfrp_app")),
                    Shape::TwoApps => {
                        make(&stage.join("First.app/Contents/MacOS/rustfrp_app"));
                        make(&stage.join("Second.app/Contents/MacOS/rustfrp_app"));
                    }
                    Shape::MissingExecutable => {
                        std::fs::create_dir(stage.join("RustFRP.app")).unwrap()
                    }
                    Shape::MissingDirectory => std::fs::remove_dir(&stage).unwrap(),
                    #[cfg(unix)]
                    Shape::SymlinkExecutable => {
                        let path = if program.ends_with("tar") {
                            stage.join("rustfrp-linux-x86_64/app/rustfrp_app")
                        } else {
                            stage.join("RustFRP.app/Contents/MacOS/rustfrp_app")
                        };
                        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                        std::os::unix::fs::symlink("/bin/sh", path).unwrap();
                    }
                    _ => {}
                }
            }
            Ok(process::Output {
                success: !matches!(
                    (signature, self.shape),
                    (false, Shape::ExtractionFailure) | (true, Shape::SignatureFailure)
                ),
                stdout: String::new(),
                code: Some(0),
            })
        }
        fn shell(&self, path: &Path) -> Result<(), Error> {
            let script = std::fs::read_to_string(path).unwrap();
            assert!(script.contains(&wait_for_exit(std::process::id())));
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                    0o700
                );
            }
            self.shells.lock().unwrap().push(script);
            if self.fail_shell {
                Err(Error::io("installer could not start"))
            } else {
                Ok(())
            }
        }
        fn windows(&self, package: &Path, executable: &Path) -> Result<(), Error> {
            self.windows
                .lock()
                .unwrap()
                .push((package.into(), executable.into()));
            Ok(())
        }
    }
    fn fixture(directory: &Path, method: Method) -> (Plan, PathBuf) {
        let download = tempfile::Builder::new()
            .prefix("download-")
            .tempdir_in(directory)
            .unwrap()
            .keep();
        let package = download.join("package");
        std::fs::write(&package, b"verified package").unwrap();
        (
            Plan {
                asset: Some("package".into()),
                manual: None,
                method,
            },
            package,
        )
    }
    #[test]
    fn handoff_validates_mac_package_signature_before_starting_private_installer_and_cleans_failed_stages(
    ) {
        let directory = tempfile::tempdir().unwrap();
        let bundle = directory.path().join("Existing 'app.app");
        std::fs::create_dir(&bundle).unwrap();
        std::fs::write(bundle.join("version"), "old").unwrap();
        for shape in [
            Shape::Mac,
            Shape::Empty,
            Shape::TwoApps,
            Shape::MissingExecutable,
            Shape::SymlinkExecutable,
            Shape::MissingDirectory,
            Shape::ExtractionFailure,
            Shape::ExtractionError,
            Shape::SignatureFailure,
            Shape::SignatureError,
        ] {
            let (plan, package) = fixture(
                directory.path(),
                Method::Mac {
                    bundle: bundle.clone(),
                },
            );
            let driver = Recorded::new(shape);
            let result = handoff_with(&plan, &package, &driver);
            if matches!(shape, Shape::Mac) {
                assert!(result.is_ok());
                let scripts = driver.shells.lock().unwrap();
                assert_eq!(scripts.len(), 1);
                assert!(scripts.first().unwrap().contains("/usr/bin/open"));
                assert!(scripts.first().unwrap().contains(".old.$$"));
                assert!(driver
                    .stages
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|path| path.is_dir()));
            } else {
                assert!(result.is_err(), "{shape:?}");
                assert!(driver.shells.lock().unwrap().is_empty());
                assert!(driver
                    .stages
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|path| !path.exists()));
            }
            assert_eq!(
                std::fs::read_to_string(bundle.join("version")).unwrap(),
                "old"
            );
        }
        let (plan, package) = fixture(
            directory.path(),
            Method::Mac {
                bundle: bundle.clone(),
            },
        );
        let mut driver = Recorded::new(Shape::Mac);
        driver.fail_shell = true;
        assert!(handoff_with(&plan, &package, &driver).is_err());
        assert!(driver
            .stages
            .lock()
            .unwrap()
            .iter()
            .all(|path| !path.exists()));
        let (plan, package) = fixture(
            directory.path(),
            Method::Mac {
                bundle: bundle.clone(),
            },
        );
        std::fs::write(
            package.parent().unwrap().join("install.sh"),
            "original helper",
        )
        .unwrap();
        let driver = Recorded::new(Shape::Mac);
        assert!(handoff_with(&plan, &package, &driver).is_err());
        assert_eq!(
            std::fs::read_to_string(package.parent().unwrap().join("install.sh")).unwrap(),
            "original helper"
        );
        assert!(driver.shells.lock().unwrap().is_empty());
        assert!(driver
            .stages
            .lock()
            .unwrap()
            .iter()
            .all(|path| !path.exists()));
        for bundle in [
            PathBuf::from("/"),
            directory.path().join("missing/RustFRP.app"),
        ] {
            let (plan, package) = fixture(directory.path(), Method::Mac { bundle });
            assert!(handoff_with(&plan, &package, &Recorded::new(Shape::Mac)).is_err());
        }
    }
    #[test]
    fn linux_and_windows_handoff_follow_package_strategy_and_reject_paths_and_missing_files() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("installation");
        std::fs::create_dir(&root).unwrap();
        for shape in [
            Shape::Tar,
            Shape::Empty,
            Shape::ExtractionFailure,
            Shape::ExtractionError,
            #[cfg(unix)]
            Shape::SymlinkExecutable,
        ] {
            let (plan, package) = fixture(
                directory.path(),
                Method::Tar {
                    root: root.clone(),
                    relative: "app/rustfrp_app".into(),
                },
            );
            let driver = Recorded::new(shape);
            let result = handoff_with(&plan, &package, &driver);
            if matches!(shape, Shape::Tar) {
                assert!(result.is_ok());
                assert!(driver
                    .shells
                    .lock()
                    .unwrap()
                    .first()
                    .unwrap()
                    .contains("setsid"));
            } else {
                assert!(result.is_err());
                assert!(driver.shells.lock().unwrap().is_empty());
                assert!(driver
                    .stages
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|path| !path.exists()));
            }
        }
        for relative in ["", "../app/rustfrp_app", "/bin/sh"] {
            let (plan, package) = fixture(
                directory.path(),
                Method::Tar {
                    root: root.clone(),
                    relative: relative.into(),
                },
            );
            assert!(handoff_with(&plan, &package, &Recorded::new(Shape::Tar)).is_err());
        }
        for root in [PathBuf::from("/"), directory.path().join("missing/app")] {
            let (plan, package) = fixture(
                directory.path(),
                Method::Tar {
                    root,
                    relative: "app/rustfrp_app".into(),
                },
            );
            assert!(handoff_with(&plan, &package, &Recorded::new(Shape::Tar)).is_err());
        }
        let (plan, package) = fixture(
            directory.path(),
            Method::Deb {
                executable: "/usr/bin/rustfrp_app".into(),
            },
        );
        let driver = Recorded::new(Shape::Empty);
        handoff_with(&plan, &package, &driver).unwrap();
        let scripts = driver.shells.lock().unwrap();
        assert_eq!(scripts.len(), 1);
        assert!(scripts.first().unwrap().contains("pkexec dpkg -i"));
        assert!(scripts.first().unwrap().contains("DEBIAN_FRONTEND"));
        drop(scripts);
        let application = directory.path().join("installed app/rustfrp_app.exe");
        let (plan, package) = fixture(
            directory.path(),
            Method::Windows {
                executable: application.clone(),
            },
        );
        let driver = Recorded::new(Shape::Empty);
        handoff_with(&plan, &package, &driver).unwrap();
        assert_eq!(
            *driver.windows.lock().unwrap(),
            vec![(package.clone(), application.clone())]
        );
        assert!(driver.shells.lock().unwrap().is_empty());
        assert!(handoff_with(&plan, &directory.path().join("missing"), &driver).is_err());
        assert!(handoff_with(&plan, Path::new("relative"), &driver).is_err());
        assert!(handoff_with(&plan, directory.path(), &driver).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let script = directory.path().join("valid-install.sh");
            std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
            spawn_script(&script).unwrap();
        }
        #[cfg(unix)]
        {
            let link = directory.path().join("symlink");
            std::os::unix::fs::symlink(&package, &link).unwrap();
            assert!(handoff_with(&plan, &link, &driver).is_err());
        }
        #[cfg(not(windows))]
        assert!(Native.windows(&package, &application).is_err());
        let (plan, package) = fixture(directory.path(), Method::Manual);
        assert!(handoff(&plan, &package).is_err());
    }
    #[test]
    #[cfg(unix)]
    fn actual_native_unpack_failure_and_shell_start_have_observable_results_without_replacing_an_application(
    ) {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("unchanged");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("version"), "old").unwrap();
        let (plan, package) = fixture(
            directory.path(),
            if cfg!(target_os = "macos") {
                Method::Mac {
                    bundle: target.clone(),
                }
            } else {
                Method::Tar {
                    root: target.clone(),
                    relative: "app/rustfrp_app".into(),
                }
            },
        );
        assert!(handoff(&plan, &package).is_err());
        assert_eq!(
            std::fs::read_to_string(target.join("version")).unwrap(),
            "old"
        );
        let helper = directory.path().join("helper.sh");
        let done = directory.path().join("done");
        std::fs::write(&helper, format!("printf started > {}\n", quote(&done))).unwrap();
        Native.shell(&helper).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !done.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(std::fs::read_to_string(done).unwrap(), "started");
        assert!(Native
            .shell(Path::new("/nonexistent/rustfrp-helper"))
            .is_err());
    }
    #[test]
    fn package_plans_match_existing_release_names_and_preserve_manual_installation_boundaries() {
        let assets = [
            "rustfrp-macos-arm64.zip",
            "rustfrp-macos-x86_64.zip",
            "rustfrp-linux-x86_64.tar.gz",
            "rustfrp-linux-x86_64.deb",
            "rustfrp-windows-x86_64-setup.exe",
        ]
        .map(String::from);
        let mac = Path::new("/Applications/RustFRP.app/Contents/MacOS/rustfrp_app");
        assert!(matches!(
            plan(Platform::Mac, "aarch64", mac, false, &assets, |_| true).method,
            Method::Mac { .. }
        ));
        for (arch, path, writable) in [
            ("unknown", mac, true),
            (
                "arm64",
                Path::new("/Volumes/Disk/RustFRP.app/Contents/MacOS/rustfrp_app"),
                true,
            ),
            ("arm64", Path::new("/tmp/plain"), true),
            ("arm64", mac, false),
        ] {
            assert!(
                plan(Platform::Mac, arch, path, false, &assets, |_| writable)
                    .manual
                    .is_some()
            );
        }
        assert!(plan(Platform::Mac, "arm64", mac, false, &[], |_| true)
            .asset
            .is_none());
        assert!(matches!(
            plan(
                Platform::Linux,
                "x86_64",
                Path::new("/usr/lib/rustfrp/app/rustfrp_app"),
                true,
                &assets,
                |_| false
            )
            .method,
            Method::Deb { .. }
        ));
        assert!(matches!(
            plan(
                Platform::Linux,
                "x86_64",
                Path::new("/opt/rustfrp/app/rustfrp_app"),
                false,
                &assets,
                |_| true
            )
            .method,
            Method::Tar { .. }
        ));
        assert!(plan(
            Platform::Linux,
            "arm64",
            Path::new("/opt/app/bin"),
            false,
            &assets,
            |_| true
        )
        .manual
        .is_some());
        assert!(plan(
            Platform::Linux,
            "x86_64",
            Path::new("/tmp/other/bin"),
            false,
            &assets,
            |_| true
        )
        .manual
        .is_some());
        assert!(plan(
            Platform::Linux,
            "x86_64",
            Path::new("/"),
            false,
            &assets,
            |_| true
        )
        .manual
        .is_some());
        assert!(plan(
            Platform::Linux,
            "x86_64",
            Path::new("/app"),
            false,
            &assets,
            |_| true
        )
        .manual
        .is_some());
        assert!(current(Path::new("/nonstandard/not-in-a-bundle"), &assets)
            .manual
            .is_some());
        assert!(
            current(Path::new("/usr/lib/rustfrp/app/rustfrp_app"), &assets)
                .manual
                .is_some()
        );
        assert!(current(Path::new("/usr/bin/rustfrp_app"), &assets)
            .manual
            .is_some());
        assert!(plan(
            Platform::Mac,
            "arm64",
            Path::new("/Applications/RustFRP.app/Contents/MacOS/app\n"),
            false,
            &assets,
            |_| true
        )
        .manual
        .is_some());
        assert!(plan(
            Platform::Mac,
            "arm64",
            Path::new(
                "/private/var/folders/AppTranslocation/RustFRP.app/Contents/MacOS/rustfrp_app"
            ),
            false,
            &assets,
            |_| true
        )
        .manual
        .is_some());
        assert!(matches!(
            plan(
                Platform::Linux,
                "x86_64",
                Path::new("/opt/rustfrp/app/rustfrp_app"),
                true,
                &["rustfrp-linux-x86_64.tar.gz".into()],
                |_| true
            )
            .method,
            Method::Tar { .. }
        ));
        assert!(plan(
            Platform::Linux,
            "x86_64",
            Path::new("/opt/rustfrp/app/rustfrp_app"),
            true,
            &[],
            |_| true
        )
        .manual
        .is_some());
        assert!(matches!(
            plan(
                Platform::Windows,
                "x86_64",
                Path::new("/installed/app.exe"),
                true,
                &assets,
                |_| true
            )
            .method,
            Method::Windows { executable } if executable == Path::new("/installed/app.exe")
        ));
        assert!(plan(
            Platform::Windows,
            "x86_64",
            Path::new("/portable/app.exe"),
            false,
            &assets,
            |_| true
        )
        .manual
        .is_some());
        assert!(plan(
            Platform::Windows,
            "arm64",
            Path::new("/installed/app.exe"),
            true,
            &assets,
            |_| true
        )
        .manual
        .is_some());
        assert!(plan(
            Platform::Windows,
            "x86_64",
            Path::new("/installed/app.exe"),
            true,
            &[],
            |_| true
        )
        .manual
        .is_some());
        assert!(plan(
            Platform::Linux,
            "x86_64",
            Path::new("relative"),
            false,
            &assets,
            |_| true
        )
        .manual
        .is_some());
        let directory = tempfile::tempdir().unwrap();
        assert!(writable(directory.path()));
        assert!(!writable(&directory.path().join("missing")));
        assert!(!installed(
            Platform::Windows,
            Path::new("/portable/rustfrp_app.exe")
        ));
        let uninstall = directory.path().join("uninstall.exe");
        std::fs::write(&uninstall, b"uninstaller").unwrap();
        assert!(installed(
            Platform::Windows,
            &directory.path().join("rustfrp_app.exe")
        ));
        std::fs::remove_file(uninstall).unwrap();
        let old_uninstall = directory.path().join("unins000.exe");
        std::fs::write(&old_uninstall, b"uninstaller").unwrap();
        assert!(installed(
            Platform::Windows,
            &directory.path().join("rustfrp_app.exe")
        ));
        assert!(installed(
            Platform::Linux,
            Path::new("/usr/lib/rustfrp/app/rustfrp_app")
        ));
        assert!(installed(
            Platform::Linux,
            Path::new("/usr/bin/rustfrp_app")
        ));
        assert!(!installed(
            Platform::Mac,
            Path::new("/Applications/RustFRP.app/Contents/MacOS/rustfrp_app")
        ));
    }
    #[test]
    #[cfg(unix)]
    fn failed_application_restart_restores_the_original_and_quotes_shell_metacharacters() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("old 'app;$name");
        let fresh = directory.path().join("fresh app");
        let stage = directory.path().join("stage");
        let download = directory.path().join("download");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("version"), "old").unwrap();
        std::fs::create_dir_all(&fresh).unwrap();
        std::fs::write(fresh.join("version"), "new").unwrap();
        let executable = target.join("does-not-exist");
        let script = replace_script(&target, &fresh, &executable, &stage, &download, false);
        let path = directory.path().join("install.sh");
        std::fs::write(&path, script).unwrap();
        let output = process::run(
            "/bin/sh",
            &[path.to_string_lossy().into_owned()],
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(!output.success);
        assert_eq!(
            std::fs::read_to_string(target.join("version")).unwrap(),
            "old"
        );
        assert!(!fresh.exists());
        assert!(wait_for_exit(42).contains("kill -0 42 2>/dev/null && exit 1"));
        assert!(handoff(
            &Plan {
                asset: None,
                manual: None,
                method: Method::Manual
            },
            &directory.path().join("pkg")
        )
        .is_err());
    }
}
