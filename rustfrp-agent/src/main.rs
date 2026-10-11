//! 入口：只做参数解析、运行时搭建与信号处理，逻辑都在库里（见 tools/cov/exclusions.md）。

use rustfrp_agent::{app, cli};
use rustfrp_core::frpcfg::{StdEnv, StdFs};
use tokio_util::sync::CancellationToken;

fn main() {
    let mut argv = std::env::args();
    let argv0 = argv.next().unwrap_or_default();
    let args: Vec<String> = argv.collect();
    let Ok(cmd) = cli::parse_args(&argv0, &args).inspect_err(|e| eprintln!("{e}\n\n{}", cli::HELP))
    else {
        std::process::exit(2);
    };
    let workers = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(2);
    let Ok(rt) = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .enable_all()
        .build()
    else {
        std::process::exit(1);
    };
    let cancel = CancellationToken::new();
    let c = cancel.clone();
    let code = rt.block_on(async move {
        tokio::spawn(async move {
            app::wait_for_stop().await;
            c.cancel();
        });
        app::execute(
            cmd,
            &StdFs,
            &StdEnv,
            &mut std::io::stdout(),
            &mut std::io::stderr(),
            cancel,
        )
        .await
    });
    std::process::exit(code);
}
