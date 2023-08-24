use crate::{apply_changes, backstage, Change, ChangeSet, Context, Output, argocd};
use anyhow::{Context as anyhowContext, Result};
use log::info;
use octocrab::{models::{Repository, Code}, Page, params::Direction};
use prettydiff::{text::ContextConfig, diff_lines};
use regex::Regex;


pub(crate) async fn enrich_catalog_files(ctx: &Context) -> Result<()> {
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
