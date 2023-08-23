use anyhow::{bail, Context as anyhowContext, Result};
use clap::Parser;
use itertools::Itertools;
use octocrab::{
    models::{repos::Object, Code, Repository},
    params::repos::Reference,
};
use prettydiff::{diff_lines, text::ContextConfig};
use regex::Regex;
use std::{collections::HashMap, env};

mod backstage;
mod github;

use crate::github::GithubClient;

#[derive(Debug)]
struct ChangeSet {
    changes: Vec<Change>,
}

impl std::ops::Deref for ChangeSet {
    type Target = Vec<Change>;

    fn deref(&self) -> &Self::Target {
        &self.changes
    }
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
    CreateFile { path: String, content: String },
    UpdateFile { path: String, content: String },
}

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

    #[arg(long, default_value_t = false)]
    foo: bool,
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

async fn create_catalog_entry(ctx: &Context, repo: &Repository) -> Result<ChangeSet> {
    let owner = &ctx.options.org;

    let has_argo = ctx
        .client
        .repos(owner, &repo.name)
        .get_content()
        .path(".argocd.yaml")
        .send()
        .await
        .context(format!("checking {}", repo.name))
        .is_ok();

    let mut entry =
        backstage::Component::new(&repo.name, repo.description.clone().unwrap_or_default());

    entry.metadata.annotations.insert(
        "github.com/project-slug".to_owned(),
        repo.full_name.to_owned().unwrap_or_default(),
    );

    if has_argo {
        entry
            .metadata
            .annotations
            .insert("aargocd/app-name".to_owned(), repo.name.to_owned());
    }

    Ok(Change::CreateFile {
        path: "catalog-info.yaml".to_owned(),
        content: serde_yaml::to_string(&entry).unwrap(),
    }
    .into())
}

async fn create_missing_catalog_files(ctx: &Context) -> Result<()> {
    let owner = &ctx.options.org;
    let repos = ctx.client.orgs(owner).list_repos().send().await?;
    for repo in ctx.client.all_pages(repos).await? {
        if let Some(true) = repo.archived {
            // println!("{} is archived", &repo.name);
            continue;
        }

        let has_catalog = ctx
            .client
            .repos(owner, &repo.name)
            .get_content()
            .path("catalog-info.yaml")
            .send()
            .await
            .context(format!("checking {}", repo.name));

        if has_catalog.is_err() {
            println!("{} does not have catalog-info.yaml", &repo.name);
            let changeset = create_catalog_entry(ctx, &repo).await?;
            println!("{:#?}", changeset)
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let ctx = Context::new(
        GithubClient::new(env::var("GITHUB_TOKEN").expect("GITHUB_TOKEN not set")),
        Args::parse(),
    );

    if ctx.options.foo {
        let mut results: Vec<Output> = Vec::new();

        for file in find_files(&ctx).await?.into_iter() {
            match find_and_replace_in_repo(&ctx, file).await {
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
    } else {
        create_missing_catalog_files(&ctx).await?;
    }

    Ok(())
}

enum Output {
    PullRequest { url: String },
    Skipped(),
    DryRun(),
}

async fn find_and_replace_in_repo(
    ctx: &Context,
    (repo, files): (String, Vec<Code>),
) -> Result<Output> {
    let owner = &ctx.options.org;
    let find = &ctx.options.find;
    let replace = &ctx.options.replace;

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

    let mut changes = ChangeSet::new();

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

        changes.add(Change::UpdateFile {
            path: path.to_owned(),
            content: replaced,
        });
    }

    apply_changes(
        ctx,
        &repo,
        changes,
        "[no-ci] chore: Replace `{find}` with `{replace}`".to_owned(),
    )
    .await
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

    ctx.client
        .delete_ref_if_exists(
            owner,
            repo_name,
            &Reference::Branch(branch_name.to_string()),
        )
        .await?;

    let branch = ctx
        .client
        .branch_from_ref(
            owner,
            repo_name,
            branch_name,
            &Reference::Branch(default_branch.to_string()),
        )
        .await
        .context(format!("Creating remote branch {branch_name}"))?;

    let sha = match branch.object {
        Object::Commit { sha, url: _ } => sha,
        Object::Tag { sha, url: _ } => sha,
        _ => bail!("could not get sha for ref {:?}", branch.object),
    };

    if !should_write {
        ctx.client
            .delete_ref_if_exists(
                owner,
                repo_name,
                &Reference::Branch(branch_name.to_string()),
            )
            .await?;
        return Ok(Output::DryRun());
    }

    for change in changes.changes {
        match change {
            Change::CreateFile { path, content } => {
                ctx.client
                    .repos(owner, repo_name)
                    .create_file(&path, format!("[no ci] chore: Create {}", &path), content)
                    .branch(branch_name)
                    .send()
                    .await?;
            }
            Change::UpdateFile { path, content } => {
                ctx.client
                    .repos(owner, repo_name)
                    .update_file(
                        &path,
                        format!("[no ci] chore: Update {}", &path),
                        content,
                        &sha,
                    )
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
