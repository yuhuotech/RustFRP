//! 入口：参数解析、运行时搭建与信号处理；逻辑都在库里（见 tools/cov/exclusions.md）。

use rustfrp_server::cli;
use tokio_util::sync::CancellationToken;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Ok(cmd) = cli::parse(&args).inspect_err(|e| eprintln!("error: {e}\n\n{}", cli::HELP))
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
            let _ = tokio::signal::ctrl_c().await;
            c.cancel();
        });
        let env = |k: &str| std::env::var(k).ok();
        let stdin = std::io::stdin();
        cli::execute(
            cmd,
            &env,
            &mut stdin.lock(),
            &mut std::io::stdout(),
            &mut std::io::stderr(),
            cancel,
        )
        .await
    });
    std::process::exit(code);
}
