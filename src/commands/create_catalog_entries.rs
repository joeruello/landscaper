use crate::{apply_changes, backstage, Change, ChangeSet, Context, Output};
use anyhow::{Context as anyhowContext, Result};
use log::info;
use octocrab::models::Repository;
use regex::Regex;

pub(crate) async fn create_missing_catalog_files(ctx: &Context) -> Result<()> {
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
