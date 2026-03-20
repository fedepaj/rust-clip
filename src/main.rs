#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use rust_clip::core::identity::RingIdentity;
use rust_clip::node::RustClipNode;
use clap::{Parser, Subcommand};

#[cfg(target_os = "windows")]
use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    New,
    Join,
    Start,
}

fn main() -> anyhow::Result<()> {
    let args = Cli::parse();
    if args.command.is_some() {
        attach_console_if_windows();
    }

    match args.command {
        Some(Commands::Start) | None => run_node()?,
        Some(Commands::New) => {
            let _ = RingIdentity::create_new()?;
        }
        Some(Commands::Join) => {
            print!("Enter ring mnemonic: ");
            use std::io::{self, Write};
            io::stdout().flush()?;
            let mut phrase = String::new();
            io::stdin().read_line(&mut phrase)?;
            let id = RingIdentity::from_mnemonic(phrase.trim())?;
            id.save()?;
        }
    }

    Ok(())
}

fn run_node() -> anyhow::Result<()> {
    let identity = RingIdentity::load().unwrap_or_else(|_| {
        RingIdentity::create_new().expect("Failed to create identity")
    });

    let node = RustClipNode::new(identity);
    node.run()
}

fn attach_console_if_windows() {
    #[cfg(target_os = "windows")]
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}
