extern crate log;

use anyhow::{Context as anyhowContext, Result};
use clap::Parser;
use cli::{Command, GlobalOpts};
use octocrab::{models::Repository, params::repos::Reference};
use std::env;

mod argocd;
mod backstage;
mod cli;
mod commands;
mod github;

use crate::github::GithubClient;

#[derive(Debug)]
struct ChangeSet {
    changes: Vec<Change>,
}

impl ChangeSet {
    fn new() -> Self {
        Self { changes: vec![] }
    }

    fn add(&mut self, change: Change) {
        self.changes.push(change);
    }
}

impl From<Change> for ChangeSet {
    fn from(change: Change) -> Self {
        Self {
            changes: vec![change],
        }
    }
}
#[derive(Debug)]
enum Change {
    CreateFile {
        path: String,
        content: String,
    },
    UpdateFile {
        path: String,
        content: String,
        sha: String,
    },
}

struct Context {
    client: GithubClient,
    options: GlobalOpts,
}

impl Context {
    fn new(client: GithubClient, options: GlobalOpts) -> Self {
        Self { client, options }
    }
}

enum Output {
    PullRequest { url: String },
    Skipped(),
    DryRun(),
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    let args = cli::App::parse();
    let ctx = Context::new(
        GithubClient::new(env::var("GITHUB_TOKEN").expect("GITHUB_TOKEN not set")),
        args.global_opts,
    );

    match args.command {
        Command::FindReplace(args) => {
            commands::find_and_replace_in_org(&ctx, &args).await?;
        }
        Command::CreateCatalogFiles {} => {
            commands::create_missing_catalog_files(&ctx).await?;
        }
        Command::EnrichCatalogFiles {} => {
            commands::enrich_catalog_files(&ctx).await?;
        }
    }

    Ok(())
}

async fn apply_changes(
    ctx: &Context,
    repo: &Repository,
    changes: ChangeSet,
    title: String,
) -> Result<Output> {
    let owner = &ctx.options.org;
    let should_write = ctx.options.write;
    let branch_name = &ctx.options.branch;
    let repo_name = &repo.name;

    let default_branch = repo
        .default_branch
        .to_owned()
        .context(format!("No default branch for {owner}/{repo_name}"))?;

    if !should_write {
        return Ok(Output::DryRun());
    }

    ctx.client
        .delete_ref_if_exists(
            owner,
            repo_name,
            &Reference::Branch(branch_name.to_string()),
        )
        .await?;

    ctx.client
        .branch_from_ref(
            owner,
            repo_name,
            branch_name,
            &Reference::Branch(default_branch.to_string()),
        )
        .await
        .context(format!("Creating remote branch {branch_name}"))?;

    for change in changes.changes {
        match change {
            Change::CreateFile { path, content } => {
                ctx.client
                    .repos(owner, repo_name)
                    .create_file(&path, format!("chore: Create {}", &path), content)
                    .branch(branch_name)
                    .send()
                    .await?;
            }
            Change::UpdateFile { path, content, sha } => {
                ctx.client
                    .repos(owner, repo_name)
                    .update_file(&path, format!("chore: Update {}", &path), content, &sha)
                    .branch(branch_name)
                    .send()
                    .await?;
            }
        }
    }

    let pr = ctx
        .client
        .pulls(owner, repo_name)
        .create(title, branch_name, default_branch)
        .send()
        .await?;

    Ok(Output::PullRequest {
        url: pr
            .html_url
            .context("PR should have a html url")?
            .to_string(),
    })
}
