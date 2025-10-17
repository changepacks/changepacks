use clap::ValueEnum;

#[derive(Debug, Clone, ValueEnum)]
pub enum FilterOptions {
    Workspace,
    Package,
}
