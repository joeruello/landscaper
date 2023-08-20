use anyhow::{Context as anyhowContext, Result};
use clap::Parser;
use itertools::Itertools;
use octocrab::{models::Code, params::repos::Reference};
use prettydiff::{diff_lines, text::ContextConfig};
use regex::Regex;
use std::{collections::HashMap, env};

mod github;

use crate::github::GithubClient;

/// A tool to do find and replace operations across in github organisations
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {

    /// Flag to actually write the changes to github
    #[arg(short, long, default_value_t = false)]
    write: bool,

    /// The string to find in the code
    #[arg(short, long)]
    find: String,

    /// The string to replace the find string with
    #[arg(short, long)]
    replace: String,

    /// The github org to search
    #[arg(short, long)]
    org: String,

    /// The branch changes will be pushes to
    #[arg(short, long, default_value = "landscaper")]
    branch: String,

    /// Regex filter on the repository name, use this to only target specific repositories
    #[arg(long)]
    repo: Option<String>,
}

struct Context {
    client: GithubClient,
    options: Args,
}

impl Context {
    fn new(client: GithubClient, options: Args) -> Self {
        Self { client, options }
    }
}

async fn find_files(ctx: &Context) -> Result<HashMap<String, Vec<Code>>> {
    Ok(ctx
        .client
        .search()
        .code(&format!("org:{} {}", ctx.options.org, ctx.options.find))
        .send()
        .await?
        .into_iter()
        .into_group_map_by(|f| f.repository.name.to_owned()))
}

#[tokio::main]
async fn main() -> Result<()> {
    let ctx = Context::new(
        GithubClient::new(env::var("GITHUB_TOKEN").expect("GITHUB_TOKEN not set")),
        Args::parse(),
    );

    let mut results: Vec<Output> = Vec::new();

    for file in find_files(&ctx).await?.into_iter() {

        match process_repo(&ctx, file).await {
            Ok(n) => {
                println!("Done.");
                results.push(n);
            }
            Err(e) => {
                println!("Error: {:?}\n Skipping.", e);
            }
        }
    }

    for results in results {
        if let Output::PullRequest { url } = results {
            println!("PR: {}", url);
        }
    }

    Ok(())
}

enum Output {
    PullRequest { url: String },
    Skipped(),
    DryRun(),
}

async fn process_repo(ctx: &Context, (repo, files): (String, Vec<Code>)) -> Result<Output> {
    let should_write = ctx.options.write;
    let owner = &ctx.options.org;
    let find = &ctx.options.find;
    let replace = &ctx.options.replace;
    let branch_name = &ctx.options.branch;

    if let Some(filter) = &ctx.options.repo {
        let re = Regex::new(filter).unwrap();
        if !re.is_match(&repo) {
            println!("Skipping {}/{} as it does not match filter", owner, repo);
            return Ok(Output::Skipped());
        }
    }

    println!("Found {} references in {}/{}", files.len(), owner, repo);

    let repo = ctx
        .client
        .repos(owner, &repo)
        .get()
        .await
        .context(format!("Fetching repo {owner}/{repo}"))?;

    let repo_name = &repo.name;

    let default_branch = repo
        .default_branch
        .context(format!("No default branch for {owner}/{repo_name}"))?;

    if should_write {
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
                &Reference::Branch("master".to_string()),
            )
            .await
            .context(format!("Creating remote branch {branch_name}"))?;
    }

    let mut commits_made = false;

    for code in files {
        let path = &code.path;
        let orginal = ctx
            .client
            .get_file_content(owner, repo_name, path)
            .await
            .context(format!("Getting content for {owner}/{repo_name}/{path}"))?;

        let replaced = orginal.replace(find, replace);

        if orginal == replaced {
            println!("No content was changed in {owner}/{repo_name}/{path}, continuing");
            continue;
        }

        println!("{owner}/{repo_name}/{path}");
        println!(
            "{}",
            diff_lines(&orginal, &replaced).format_with_context(
                Some(ContextConfig {
                    context_size: 2,
                    skipping_marker: "---"
                }),
                true
            )
        );

        if should_write {
            ctx.client
                .repos(owner, repo_name)
                .update_file(
                    code.path,
                    format!("[no ci] chore: Update {}", code.name),
                    replaced,
                    &code.sha,
                )
                .branch(branch_name)
                .send()
                .await?;
            commits_made = true;
        }
    }

    if should_write && commits_made {
        let pr = ctx
            .client
            .pulls(owner, repo_name)
            .create(
                format!("[no-ci] chore: Replace `{find}` with `{replace}`"),
                branch_name,
                default_branch,
            )
            .body(format!("Replaces all instances `{find}` with `{replace}`"))
            .send()
            .await?;

        return Ok(Output::PullRequest {
            url: pr.html_url.context("PR should have a html url")?.to_string(),
        });
    } else if should_write {
        ctx.client
            .delete_ref_if_exists(
                owner,
                repo_name,
                &Reference::Branch(branch_name.to_string()),
            )
            .await?;
    }

    Ok(Output::DryRun())
}
