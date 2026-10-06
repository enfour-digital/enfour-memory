//! Offline provisioning tool. Never linked into a public runtime image.
use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
#[derive(Parser)]
struct Args {
    #[arg(long)]
    pdf: PathBuf,
    #[arg(long, default_value = "pdftotext")]
    poppler: PathBuf,
    #[arg(long)]
    output: PathBuf,
}
fn main() -> Result<()> {
    let args = Args::parse();
    let d = enfour_memory::language::dictionary::import(&args.pdf, &args.poppler, &args.output)?;
    println!("{} entries. SHA-256: {}", d.entries.len(), d.entries_sha256);
    Ok(())
}
