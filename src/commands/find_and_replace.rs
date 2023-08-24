use crate::{apply_changes, cli::FindReplaceArgs, Change, ChangeSet, Context, Output};
use anyhow::{Context as anyhowContext, Result};
use itertools::Itertools;
use octocrab::models::Code;
use prettydiff::{diff_lines, text::ContextConfig};
use regex::Regex;
use std::collections::HashMap;

pub(crate) async fn find_and_replace_in_org(ctx: &Context, args: &FindReplaceArgs) -> Result<()> {
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
