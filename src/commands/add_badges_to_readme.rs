use crate::{apply_changes, backstage, Change, ChangeSet, Context, Output};
use anyhow::{Context as anyhowContext, Result};
use log::info;
use octocrab::{
    models::Repository,
    params::Direction,
};
use prettydiff::{diff_lines, text::ContextConfig};
use regex::Regex;

pub(crate) async fn add_badges_to_readme(ctx: &Context) -> Result<()> {
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

        let changeset = add_badge_to_readme(ctx, &repo)
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
                "[ci-skip] docs: Add ownership badges to readme".to_owned(),
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

async fn add_badge_to_readme(ctx: &Context, repo: &Repository) -> Result<ChangeSet> {
    let owner = &ctx.options.org;

    let catalog_info = ctx
        .client
        .get_file_content(owner, &repo.name, "catalog-info.yaml")
        .await
        .context(format!(
            "getting catalog-info.yaml for {}/{}",
            owner, repo.name
        ))?;

    info!("{} has catalog-info.yaml", &repo.name);

    let catalog: Result<backstage::Component> =
        serde_yaml::from_str(&catalog_info.decoded_content().context(format!(
            "getting content for catalog-info.yaml for {}/{}",
            owner, repo.name
        ))?)
        .map_err(anyhow::Error::msg)
        .context(format!(
            "parsing catalog-info.yaml for {}/{}",
            owner, repo.name
        ));

    if matches!(catalog, Result::Err(_)) {
        info!("{} does not have a valid catalog-info.yaml", &repo.name);
        return Ok(ChangeSet::new());
    }

    let component = catalog
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

    let readme = ctx
        .client
        .get_file_content(owner, &repo.name, "README.md")
        .await
        .context(format!(
            "getting README.md for {}/{}",
            owner, repo.name
        ))?;

    let readme_content = readme.decoded_content().context(
        format!("getting content for README.md for {}/{}", owner, repo.name),
    )?;

    if readme_content.contains("https://backyard.k8s.hipages.com.au/api/badges/entity/") {
        info!(
            "{} already has a badge, skipping",
            &repo.name
        );
        return Ok(ChangeSet::new());
    }


    let owner = &component.spec.owner;
    let name = &component.metadata.name;
    let entity_type = component.kind;
    let modified_readme_content = format!(
        r#"[![Link to {name} in hipages Developer Portal, {entity_type}: {name}](https://backyard.k8s.hipages.com.au/api/badges/entity/default/{entity_type}/{name}/badge/pingback "Link to {name} in hipages Developer Portal")](https://backyard.k8s.hipages.com.au/catalog/default/{entity_type}/{name})
[![Entity owner badge, owner: {owner}](https://backyard.k8s.hipages.com.au/api/badges/entity/default/{entity_type}/{name}/badge/owner "Entity owner badge")](https://backyard.k8s.hipages.com.au/catalog/default/{entity_type}/{name})
{readme_content}"#);

    if readme_content != modified_readme_content {
        println!(
            "#{}:\n----\n{}\n\n",
            &repo.name,
            diff_lines(&readme_content, &modified_readme_content).format_with_context(
                Some(ContextConfig {
                    context_size: 2,
                    skipping_marker: "---"
                }),
                true
            )
        );

        let mut changes = ChangeSet::new();
        changes.add(Change::UpdateFile {
            path: readme.path,
            content: modified_readme_content,
            sha: readme.sha,
        });

        return Ok(changes);
    }

    Ok(ChangeSet::new())
}