use anyhow::{bail, Context as anyhowContext, Result};
use clap::{Args, Parser, Subcommand};
use itertools::Itertools;
use octocrab::{
    models::{repos::Object, Code, Repository},
    params::repos::Reference,
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
    CreateFile { path: String, content: String },
    UpdateFile { path: String, content: String },
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
            Change::UpdateFile { path, content } => {
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
                // println!(
                //     "Skipping {}/{} as it does not match filter",
                //     owner, repo.name
                // );
                continue;
            }
        }

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
            results.push(
                apply_changes(
                    ctx,
                    &repo,
                    changeset,
                    "chore: Add catalog-info.yaml [no-ci]".to_owned(),
                )
                .await?,
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
    let repos = ctx.client.orgs(owner).list_repos().send().await?;

    let mut results = vec![];

    for repo in ctx.client.all_pages(repos).await?.into_iter() {
        if let Some(true) = repo.archived {
            // println!("{} is archived, skipping", &repo.name);
            continue;
        }

        if let Some(filter) = &ctx.options.repo {
            let re = Regex::new(filter).unwrap();
            if !re.is_match(&repo.name) {
                // println!("Skipping {}/{} as it does not match filter", owner, repo.name);
                continue;
            }
        }
        // println!("looking at {}", repo.name);

        let changeset = update_catalog_info(ctx, &repo).await?;
        results.push(
            apply_changes(
                ctx,
                &repo,
                changeset,
                "[no-ci] chore: Update catalog.info.yaml`".to_owned(),
            )
            .await?,
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
        .await?;

    let catalog: Result<backstage::Component> =
        serde_yaml::from_str(&catalog_original).map_err(anyhow::Error::msg);

    if matches!(catalog, Result::Err(_)) {
        return Ok(ChangeSet::new());
    }

    let mut component = catalog.unwrap();

    if component.spec._type != "service" {
        return Ok(ChangeSet::new());
    }

    component.metadata.annotations.insert(
        "github.com/project-slug".to_owned(),
        repo.full_name.to_owned().unwrap_or_default(),
    );

    let argo_file = ctx
        .client
        .get_file_content(owner, &repo.name, ".argocd.yaml")
        .await;
    if argo_file.is_ok() {
        component
            .metadata
            .annotations
            .entry("argocd/app-name".to_owned())
            .or_insert(repo.name.to_owned());

        let argo_contents = argo_file?;
        let app_spec: argocd::ArgoApp = serde_yaml::from_str(&argo_contents)?;

        dbg!(&app_spec);
    
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
            .push("resource:hip-rds-mysql-prod".to_owned());
    }

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
            .push("resource:hip-rds-mysql-prod-ro".to_owned());
    }

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
            .push("resource:rabbitmq-innocent-chimp".to_owned());
    }

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
            .push("resource:kafka-prod".to_owned());
    }
    // api gateway
    if find_string_in_repo(ctx, repo, "gloo:")
        .await?
        .total_count
        .unwrap_or_default()
        > 0
    {
        component.spec.depends_on.push("component:gloo".to_owned());
    }

    let catalog_updated = serde_yaml::to_string(&component)?;

    if catalog_original != catalog_updated {
        println!(
            "#{}:\n----\n{}\n\n",
            &repo.name,
            catalog_updated // diff_lines(&catalog_original, &catalog_updated).format_with_context(
                            //     Some(ContextConfig {
                            //         context_size: 2,
                            //         skipping_marker: "---"
                            //     }),
                            //     true
                            // )
        );

        return Ok(Change::UpdateFile {
            path: "catalog-info.yaml".to_owned(),
            content: catalog_updated,
        }
        .into());
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
