use anyhow::{anyhow, Context, Result};
use http::Uri;
use octocrab::{
    map_github_error,
    models::repos::{Content, Object, Ref},
    params::repos::Reference,
    Octocrab,
};

pub(crate) struct GithubClient {
    client: Octocrab,
}

impl std::ops::Deref for GithubClient {
    type Target = Octocrab;

    fn deref(&self) -> &Self::Target {
        &self.client
    }
}

impl GithubClient {
    pub fn new(token: String) -> Self {
        let client = Octocrab::builder().personal_token(token).build().unwrap();
        Self { client }
    }

    pub async fn get_sha_for_ref(
        &self,
        owner: &str,
        repo: &str,
        reference: &Reference,
    ) -> Result<String> {
        let ref_object = self.repos(owner, repo).get_ref(reference).await?;

        match ref_object.object {
            Object::Commit { sha, url: _ } => Ok(sha),
            _ => Err(anyhow!("could not get sha for ref {}", reference)),
        }
    }

    pub async fn branch_from_ref(
        &self,
        owner: &str,
        repo: &str,
        branch_name: &str,
        reference: &Reference,
    ) -> Result<Ref> {
        self.repos(owner, repo)
            .create_ref(
                &Reference::Branch(branch_name.to_string()),
                self.get_sha_for_ref(owner, repo, reference).await?,
            )
            .await
            .map_err(anyhow::Error::from)
    }

    pub async fn get_file_content(&self, owner: &str, repo: &str, path: &str) -> Result<Content> {
        self.repos(owner, repo)
            .get_content()
            .path(path)
            .send()
            .await?
            .items
            .pop()
            .context("Getting file content")
    }

    pub async fn delete_ref_if_exists(
        &self,
        owner: &str,
        repo: &str,
        reference: &Reference,
    ) -> Result<()> {
        match self.repos(owner, repo).get_ref(reference).await {
            Ok(_) => self.delete_ref(owner, repo, reference).await,
            Err(_) => Ok(()),
        }
    }

    pub async fn delete_ref(&self, owner: &str, repo: &str, reference: &Reference) -> Result<()> {
        let route = format!("/repos/{owner}/{repo}/git/refs/{}", reference.ref_url(),);
        let uri = Uri::builder()
            .path_and_query(&route)
            .build()
            .context("buidling path")?;
        map_github_error(self._delete(uri, None::<&()>).await?)
            .await
            .map(drop)
            .context(format!("Error deleting ref {route}"))
    }
}
