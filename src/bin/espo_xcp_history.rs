//! This executable never calls Espo configuration or module initialization.
#[path = "../modules/xcp/numbering/mod.rs"]
mod numbering;
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;
#[unsafe(export_name = "malloc_conf")]
pub static MALLOC_CONF: &[u8] = b"background_thread:true,dirty_decay_ms:1000,muzzy_decay_ms:0\0";
use clap::Parser;
#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "/var/lib/espo-xcp-history")]
    data_dir: std::path::PathBuf,
    #[arg(long, default_value = "http://127.0.0.1:4100/v2")]
    core_url: String,
    #[arg(long, default_value_t = 6)]
    confirmations: u64,
    #[arg(long, default_value = "127.0.0.1:9093")]
    listen: std::net::SocketAddr,
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    numbering::service::run(args.data_dir, args.core_url, args.confirmations, args.listen).await
}
