use clap::{Args, Parser, Subcommand};

/// Super cool backyard tools
#[derive(Debug, Parser)]
#[clap(name = "landscaper", version)]
pub(crate) struct App {
    #[clap(flatten)]
    pub global_opts: GlobalOpts,

    #[clap(subcommand)]
    pub command: Command,
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub(crate) struct GlobalOpts {
    /// Flag to actually write the changes to github
    #[arg(short, long, default_value_t = false, global = true)]
    pub write: bool,

    /// The github org to search
    #[arg()]
    pub org: String,

    /// The branch changes will be pushes to
    #[arg(short, long, default_value = "landscaper", global = true)]
    pub branch: String,

    /// Regex filter on the repository name, use this to only target specific repositories
    #[arg(long, global = true)]
    pub repo: Option<String>,

    /// Skip the first n repos when processing
    #[arg(long, global = true)]
    pub skip: Option<usize>
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Find and replace a string in all files in an org
    FindReplace(FindReplaceArgs),
    /// Create missing catalog-info.yaml files in an org
    CreateCatalogFiles {},
    /// Try and fill out catalog-info.yaml files in an org
    EnrichCatalogFiles {},
    /// Add backstage badges to README.md files in an org
    AddBadgesToReadmes {},
}

#[derive(Debug, Args)]
pub(crate) struct FindReplaceArgs {
    /// The string to find in the code
    #[arg(short, long)]
    pub find: String,

    /// The string to replace the find string with
    #[arg(short, long)]
    pub replace: String,

    /// Commit message to create the pull requests with
    #[arg(short, long)]
    pub message: Option<String>,
}
