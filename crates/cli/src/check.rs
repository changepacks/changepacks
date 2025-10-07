use clap::Args;

#[derive(Args, Debug)]
#[command(about = "Check project status")]
pub struct CheckArgs {}

pub fn handle_check(args: &CheckArgs) {}
