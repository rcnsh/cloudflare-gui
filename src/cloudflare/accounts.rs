use serde::Deserialize;

use super::{ApiError, Client, Request, Result};

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Account {
    pub id: String,
    pub name: String,
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct TokenStatus {
    pub id: String,
    pub status: String,
    pub expires_on: Option<String>,
}

impl Client {
    /// Checks a user-owned token. Account-owned tokens fail this call with
    /// an auth error even when valid, so callers should treat that case as
    /// "unknown" and rely on `list_accounts` instead.
    pub async fn verify_user_token(&self) -> Result<TokenStatus> {
        Ok(self
            .json(Request::get(["user", "tokens", "verify"]))
            .await?
            .0)
    }

    pub async fn verify_account_token(&self, account_id: &str) -> Result<TokenStatus> {
        Ok(self
            .json(Request::get(["accounts", account_id, "tokens", "verify"]))
            .await?
            .0)
    }

    /// Lists every account the token can see, following page numbers.
    pub async fn list_accounts(&self) -> Result<Vec<Account>> {
        let mut out = Vec::new();
        for page in 1.. {
            let (items, info): (Vec<Account>, _) = self
                .json(
                    Request::get(["accounts"])
                        .query("page", page)
                        .query("per_page", 50),
                )
                .await?;
            let fetched = items.len();
            out.extend(items);
            let total = info.and_then(|i| i.total_count).unwrap_or(0.0) as usize;
            if fetched == 0 || out.len() >= total {
                break;
            }
        }
        Ok(out)
    }

    /// Verifies a freshly pasted token and returns the accounts it can use.
    pub async fn verify_and_list_accounts(&self) -> Result<Vec<Account>> {
        match self.verify_user_token().await {
            Ok(status) if status.status != "active" => {
                return Err(ApiError::Decode(format!("token is {}", status.status)));
            }
            Ok(_) => {}
            // Possibly an account-owned token; the account listing below is the real test.
            Err(err) if err.is_auth() => {}
            Err(err) => return Err(err),
        }
        let accounts = self.list_accounts().await?;
        if accounts.is_empty() {
            return Err(ApiError::Decode(
                "the token is valid but can't see any accounts. Give it at least one account-level permission"
                    .into(),
            ));
        }
        Ok(accounts)
    }
}

#[cfg(test)]
mod tests {
    use crate::cloudflare::testing::{MockServer, Reply};

    #[tokio::test]
    async fn lists_accounts_from_fixture() {
        let server = MockServer::start(vec![Reply::fixture(200, "accounts_list.json")]).await;
        let accounts = server.client().list_accounts().await.unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].id, "0123456789abcdef0123456789abcdef");
        assert_eq!(accounts[0].name, "Example Account");
        let req = &server.requests()[0];
        assert_eq!(req.target, "/client/v4/accounts?page=1&per_page=50");
    }

    #[tokio::test]
    async fn verify_accepts_account_tokens_that_fail_user_verify() {
        let server = MockServer::start(vec![
            Reply::fixture(401, "error_auth.json"),
            Reply::fixture(200, "accounts_list.json"),
        ])
        .await;
        let accounts = server.client().verify_and_list_accounts().await.unwrap();
        assert_eq!(accounts.len(), 1);
    }

    #[tokio::test]
    async fn verify_rejects_tokens_with_no_accounts() {
        let server = MockServer::start(vec![
            Reply::fixture(200, "token_verify.json"),
            Reply::status(200).body(r#"{"success":true,"errors":[],"messages":[],"result":[],"result_info":{"page":1,"per_page":50,"count":0,"total_count":0}}"#),
        ])
        .await;
        let err = server
            .client()
            .verify_and_list_accounts()
            .await
            .unwrap_err();
        assert!(err.to_string().contains("can't see any accounts"));
    }
}
