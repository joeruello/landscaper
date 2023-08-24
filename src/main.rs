extern crate log;

use anyhow::{Context as anyhowContext, Result};
use clap::{Args, Parser, Subcommand};
use itertools::Itertools;
use log::info;
use octocrab::{
    models::{ Code, Repository},
    params::{repos::Reference, Direction},
    Page,
};
use prettydiff::{diff_lines, text::ContextConfig};
use regex::Regex;
use std::{collections::HashMap, env};

mod argocd;
mod backstage;
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

/// Here's my app!
#[derive(Debug, Parser)]
#[clap(name = "my-app", version)]
pub struct App {
    #[clap(flatten)]
    global_opts: GlobalOpts,

    #[clap(subcommand)]
    command: Command,
}

/// A tool to do find and replace operations across in github organisations
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct GlobalOpts {
    /// Flag to actually write the changes to github
    #[arg(short, long, default_value_t = false)]
    write: bool,

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

#[derive(Debug, Subcommand)]
enum Command {
    /// Find and replace a string in all files in an org
    FindReplace(FindReplaceArgs),
    /// Create missing catalog-info.yaml files in an org
    CreateCatalogFiles {},
    EnrichCatalogFiles {},
}

#[derive(Debug, Args)]
struct FindReplaceArgs {
    /// The string to find in the code
    #[arg(short, long)]
    find: String,

    /// The string to replace the find string with
    #[arg(short, long)]
    replace: String,
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
    let args = App::parse();
    let ctx = Context::new(
        GithubClient::new(env::var("GITHUB_TOKEN").expect("GITHUB_TOKEN not set")),
        args.global_opts,
    );

    match args.command {
        Command::FindReplace(args) => {
            find_and_replace_in_org(&ctx, &args).await?;
        }
        Command::CreateCatalogFiles {} => {
            create_missing_catalog_files(&ctx).await?;
        }
        Command::EnrichCatalogFiles {} => {
            enrich_catalog_files(&ctx).await?;
        }
    }

    Ok(())
}

async fn find_and_replace_in_org(ctx: &Context, args: &FindReplaceArgs) -> Result<()> {
    let mut results: Vec<Output> = Vec::new();

    for file in find_files(ctx, args).await?.into_iter() {
        match find_and_replace_in_repo(ctx, args, file).await {
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

async fn find_files(ctx: &Context, args: &FindReplaceArgs) -> Result<HashMap<String, Vec<Code>>> {
    Ok(ctx
        .client
        .search()
        .code(&format!("org:{} {}", ctx.options.org, args.find))
        .send()
        .await?
        .into_iter()
        .into_group_map_by(|f| f.repository.name.to_owned()))
}

async fn find_and_replace_in_repo(
    ctx: &Context,
    args: &FindReplaceArgs,
    (repo, files): (String, Vec<Code>),
) -> Result<Output> {
    let owner = &ctx.options.org;
    let find = &args.find;
    let replace = &args.replace;

    if let Some(filter) = &ctx.options.repo {
        let re = Regex::new(filter).unwrap();
        if !re.is_match(&repo) {
            // println!("Skipping {}/{} as it does not match filter", owner, repo);
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
        let orginal = ctx.client.get_file_content(owner, repo_name, path).await?;
        let content = orginal
            .decoded_content()
            .context(format!("Getting content for {owner}/{repo_name}/{path}"))?;

        let replaced = content.replace(find, replace);

        if content == replaced {
            println!("No content was changed in {owner}/{repo_name}/{path}, continuing");
            continue;
        }

        println!("{owner}/{repo_name}/{path}");
        println!(
            "{}",
            diff_lines(&content, &replaced).format_with_context(
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
            sha: orginal.sha,
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
                ctx
                    .client
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
            .insert("argocd/app-name".to_owned(), repo.name.to_owned());
    }

    Ok(Change::CreateFile {
        path: "catalog-info.yaml".to_owned(),
        content: serde_yaml::to_string(&entry).unwrap(),
    }
    .into())
}

async fn create_missing_catalog_files(ctx: &Context) -> Result<()> {
    let owner = &ctx.options.org;
    let repos = ctx
        .client
        .orgs(owner)
        .list_repos()
        .sort(octocrab::params::repos::Sort::Pushed)
        .send()
        .await?;

    let mut results = vec![];

    for repo in ctx.client.all_pages(repos).await?.into_iter() {
        if let Some(filter) = &ctx.options.repo {
            let re = Regex::new(filter).unwrap();
            if !re.is_match(&repo.name) {
                info!(
                    "Skipping {}/{} as it does not match filter",
                    owner, repo.name
                );
                continue;
            }
        }

        if let Some(true) = repo.archived {
            info!("{} is archived", &repo.name);
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
            let changeset = create_catalog_entry(ctx, &repo)
                .await
                .context(format!("creating catalog-info.yaml for {}", repo.name))?;
            results.push(
                apply_changes(
                    ctx,
                    &repo,
                    changeset,
                    "chore: Add catalog-info.yaml [no-ci]".to_owned(),
                )
                .await
                .context(format!("creating PR for {}", repo.name))?,
            );
        }
    }

    for results in results {
        if let Output::PullRequest { url } = results {
            println!("PR: {}", url);
        }
    }

    Ok(())
}

async fn enrich_catalog_files(ctx: &Context) -> Result<()> {
    let owner = &ctx.options.org;
    let repos = ctx
        .client
        .orgs(owner)
        .list_repos()
        .direction(Direction::Descending)
        .sort(octocrab::params::repos::Sort::Updated)
        .send()
        .await?;

    let mut results = vec![];

    for repo in ctx.client.all_pages(repos).await?.into_iter() {
        if let Some(true) = repo.archived {
            info!("{} is archived, skipping", &repo.name);
            continue;
        }

        if let Some(filter) = &ctx.options.repo {
            let re = Regex::new(filter).unwrap();
            if !re.is_match(&repo.name) {
                info!(
                    "Skipping {}/{} as it does not match filter",
                    owner, repo.name
                );
                continue;
            }
        }
        info!("looking at {}", repo.name);

        let changeset = update_catalog_info(ctx, &repo)
            .await
            .context(format!("updating catalog-info.yaml for {}", repo.name))?;

        if changeset.changes.is_empty() {
            info!("no changes for {}", repo.name);
            continue;
        }

        results.push(
            apply_changes(
                ctx,
                &repo,
                changeset,
                "[no-ci] chore: Update catalog.info.yaml".to_owned(),
            )
            .await
            .context(format!("creating PR for {}", repo.name))?,
        );
    }

    for results in results {
        if let Output::PullRequest { url } = results {
            println!("PR: {}", url);
        }
    }

    Ok(())
}

async fn update_catalog_info(ctx: &Context, repo: &Repository) -> Result<ChangeSet> {
    let owner = &ctx.options.org;

    let catalog_original = ctx
        .client
        .get_file_content(owner, &repo.name, "catalog-info.yaml")
        .await
        .context(format!(
            "getting catalog-info.yaml for {}/{}",
            owner, repo.name
        ))?;

    let original_content = catalog_original.decoded_content().context(format!(
        "getting content for catalog-info.yaml for {}/{}",
        owner, repo.name
    ))?;

    info!("{} has catalog-info.yaml", &repo.name);

    let catalog: Result<backstage::Component> = serde_yaml::from_str(&original_content)
        .map_err(anyhow::Error::msg)
        .context(format!(
            "parsing catalog-info.yaml for {}/{}",
            owner, repo.name
        ));

    if matches!(catalog, Result::Err(_)) {
        info!("{} does not have a valid catalog-info.yaml", &repo.name);
        return Ok(ChangeSet::new());
    }

    let mut component = catalog
        .context(format!(
            "parsing catalog-info.yaml for {}/{}",
            owner, repo.name
        ))
        .unwrap();

    info!("{} has a valid catalog-info.yaml", &repo.name);

    if component.spec._type != "service" {
        info!("{} is not a service, skipping", &repo.name);
        return Ok(ChangeSet::new());
    }

    if component.metadata.name == "template" {
        component.metadata.name = repo.name.to_owned()
    }

    if component.metadata.description
        == "A template repository used to bootstrap the CICD environment with the required files"
        || component.metadata.description.is_empty()
    {
        component.metadata.description = repo.description.clone().unwrap_or_default()
    }

    component.metadata.annotations.insert(
        "github.com/project-slug".to_owned(),
        repo.full_name.to_owned().unwrap_or_default(),
    );

    let argo_file = ctx
        .client
        .get_file_content(owner, &repo.name, ".argocd.yaml")
        .await
        .context(format!("getting .argocd.yaml for {}/{}", owner, repo.name));

    info!("{} has .argocd.yaml", &repo.name);

    if argo_file.is_ok() {
        component
            .metadata
            .annotations
            .entry("argocd/app-name".to_owned())
            .or_insert(repo.name.to_owned());

        let argo_contents = argo_file?.decoded_content().unwrap_or_default();
        let app_spec: argocd::ArgoApp = serde_yaml::from_str(&argo_contents)?;

        component
            .metadata
            .annotations
            .entry("backstage.io/kubernetes-label-selector".to_string())
            .or_insert(format!("app.kubernetes.io/instance={}", repo.name));

        component
            .metadata
            .annotations
            .entry("backstage.io/kubernetes-namespace".to_string())
            .or_insert(app_spec.spec.destination.namespace.to_owned());
    }

    // legacy DB

    if find_string_in_repo(ctx, repo, "notmidship-db")
        .await?
        .total_count
        .unwrap_or_default()
        > 0
    {
        component
            .spec
            .depends_on
            .insert("resource:hip-rds-mysql-prod".to_owned());
    }

    info!("{} has notmidship-db", &repo.name);

    // legacy read only DB
    if find_string_in_repo(ctx, repo, "notmidship-ro-db")
        .await?
        .total_count
        .unwrap_or_default()
        > 0
    {
        component
            .spec
            .depends_on
            .insert("resource:hip-rds-mysql-prod-ro".to_owned());

    }

    info!("{} has notmidship-ro-db", &repo.name);

    // rabbitmq
    if find_string_in_repo(ctx, repo, "innocent-chimp")
        .await?
        .total_count
        .unwrap_or_default()
        > 0
    {
        component
            .spec
            .depends_on
            .insert("resource:rabbitmq-innocent-chimp".to_owned());
    }

    info!("{} has rabbitmq", &repo.name);

    // kafka
    if find_string_in_repo(ctx, repo, "kafka-prod")
        .await?
        .total_count
        .unwrap_or_default()
        > 0
    {
        component
            .spec
            .depends_on
            .insert("resource:kafka-prod".to_owned());
    }

    info!("{} api gateway", &repo.name);
    // api gateway
    if find_string_in_repo(ctx, repo, "gloo:")
        .await?
        .total_count
        .unwrap_or_default()
        > 0
    {
        component.spec.depends_on.insert("component:gloo".to_owned());
    }

    info!("{} has gloo", &repo.name);

    let catalog_updated = serde_yaml::to_string(&component).context(format!(
        "serializing catalog-info.yaml for {}/{}",
        owner, repo.name
    ))?;

    if original_content != catalog_updated {
        println!(
            "#{}:\n----\n{}\n\n",
            &repo.name,
            // catalog_updated
            diff_lines(&original_content, &catalog_updated).format_with_context(
                Some(ContextConfig {
                    context_size: 2,
                    skipping_marker: "---"
                }),
                true
            )
        );
        info!("waiting or rate limit");

        let mut changes = ChangeSet::new();
        changes.add(Change::UpdateFile {
            path: catalog_original.path,
            content: catalog_updated,
            sha: catalog_original.sha,
        });

        return Ok(changes);
    }

    Ok(ChangeSet::new())
}

async fn find_string_in_repo(ctx: &Context, repo: &Repository, needle: &str) -> Result<Page<Code>> {
    ctx.client
        .search()
        .code(&format!(
            "repo:{}/{} {}",
            ctx.options.org, repo.name, needle
        ))
        .send()
        .await
        .map_err(anyhow::Error::from)
}
