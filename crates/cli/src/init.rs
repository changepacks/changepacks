use clap::Args;

#[derive(Args, Debug)]
#[command(about = "Initialize a new Changepack project")]
pub struct InitArgs {}

pub fn handle_init(args: &InitArgs) {}
