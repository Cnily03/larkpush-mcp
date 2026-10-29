use anyhow::{Context, Result, bail};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::LarkClient;

#[derive(Debug, Deserialize)]
pub struct ListUsers {
    #[serde(default)]
    pub max_pages: Option<u8>,
    #[serde(default)]
    pub page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SearchUsers {
    pub query: String,
    #[serde(flatten)]
    pub list: ListUsers,
}

enum EmployeeQuery {
    List,
    Search(String),
}

impl EmployeeQuery {
    /// Select the directory endpoint for listing or searching employees.
    fn path(&self) -> &'static str {
        match self {
            Self::List => "directory/v1/employees/filter",
            Self::Search(_) => "directory/v1/employees/search",
        }
    }

    /// Request only the employee ID field and include pagination when supplied.
    fn body(&self, page_token: Option<&str>) -> Value {
        // Open IDs need no additional field scope; names and departments do.
        let mut body = json!({
            "required_fields": ["base_info.employee_id"],
            "page_request": {"page_size": 50},
        });
        if let Some(token) = page_token {
            body["page_request"]["page_token"] = json!(token);
        }
        match self {
            Self::List => body["filter"] = json!({"conditions": []}),
            Self::Search(query) => body["query"] = json!(query),
        }
        body
    }
}

impl LarkClient {
    /// Lists app-visible employees without fetching department or profile fields.
    pub async fn list_users(&self, args: ListUsers) -> Result<Value> {
        self.directory_users(EmployeeQuery::List, args).await
    }

    /// Searches app-visible employees using Lark's directory search API.
    pub async fn search_users(&self, args: SearchUsers) -> Result<Value> {
        if args.query.trim().is_empty() {
            bail!("query is required");
        }
        self.directory_users(EmployeeQuery::Search(args.query), args.list)
            .await
    }

    /// Collect at most `max_pages` pages and preserve the next page token.
    async fn directory_users(&self, operation: EmployeeQuery, args: ListUsers) -> Result<Value> {
        let limit = args.max_pages.unwrap_or(20);
        if limit == 0 || limit > 100 {
            bail!("max_pages must be between 1 and 100");
        }
        let mut users = Vec::new();
        let mut page_token = args.page_token;
        let mut has_more = false;
        for _ in 0..limit {
            let body = operation.body(page_token.as_deref());
            let data = self
                .request(Method::POST, operation.path(), &[], Some(body))
                .await?;
            users.extend(employee_ids(&data)?);
            has_more = data["page_response"]["has_more"]
                .as_bool()
                .context("Lark directory response is missing has_more")?;
            page_token = data["page_response"]["page_token"]
                .as_str()
                .map(str::to_owned);
            if !has_more {
                break;
            }
            if page_token.is_none() {
                bail!("Lark returned has_more without page_token");
            }
        }
        Ok(
            json!({"items": users, "has_more": has_more, "next_page_token": if has_more { page_token } else { None }}),
        )
    }

    /// List chats visible to the app bot with optional pagination.
    pub async fn list_chats(
        &self,
        page_size: Option<u8>,
        page_token: Option<String>,
    ) -> Result<Value> {
        let size = page_size.unwrap_or(50);
        if !(1..=100).contains(&size) {
            bail!("page_size must be between 1 and 100");
        }
        let mut query = vec![("page_size", size.to_string())];
        if let Some(token) = page_token {
            query.push(("page_token", token));
        }
        self.request(Method::GET, "im/v1/chats", &query, None).await
    }

    /// List members of one chat, defaulting member IDs to Open IDs.
    pub async fn list_chat_members(
        &self,
        chat_id: &str,
        member_id_type: Option<String>,
        page_size: Option<u8>,
        page_token: Option<String>,
    ) -> Result<Value> {
        if chat_id.is_empty()
            || !chat_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            bail!("invalid chat_id");
        }
        let size = page_size.unwrap_or(50);
        if !(1..=100).contains(&size) {
            bail!("page_size must be between 1 and 100");
        }
        let mut query = vec![
            (
                "member_id_type",
                member_id_type.unwrap_or_else(|| "open_id".into()),
            ),
            ("page_size", size.to_string()),
        ];
        if let Some(token) = page_token {
            query.push(("page_token", token));
        }
        self.request(
            Method::GET,
            &format!("im/v1/chats/{chat_id}/members"),
            &query,
            None,
        )
        .await
    }
}

/// Reduce directory results to Open IDs and reject missing identifiers.
fn employee_ids(data: &Value) -> Result<Vec<Value>> {
    data["employees"]
        .as_array()
        .context("Lark directory response is missing employees")?
        .iter()
        .map(|employee| {
            let open_id = employee["base_info"]["employee_id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .context("Lark directory employee is missing an open_id")?;
            Ok(json!({"open_id": open_id}))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{EmployeeQuery, employee_ids};

    #[test]
    fn directory_requests_only_open_ids() {
        let list = EmployeeQuery::List.body(None);
        assert_eq!(EmployeeQuery::List.path(), "directory/v1/employees/filter");
        assert_eq!(
            list,
            json!({
                "filter": {"conditions": []},
                "required_fields": ["base_info.employee_id"],
                "page_request": {"page_size": 50},
            })
        );

        let search = EmployeeQuery::Search("Alice".into());
        assert_eq!(search.path(), "directory/v1/employees/search");
        assert_eq!(
            search.body(Some("next")),
            json!({
                "query": "Alice",
                "required_fields": ["base_info.employee_id"],
                "page_request": {"page_size": 50, "page_token": "next"},
            })
        );
    }

    #[test]
    fn extracts_only_open_ids_from_directory_response() {
        let data = json!({"employees": [{"base_info": {"employee_id": "ou_1", "name": "Alice"}}]});
        assert_eq!(
            employee_ids(&data).unwrap(),
            vec![json!({"open_id": "ou_1"})]
        );
        assert!(employee_ids(&json!({"employees": [{}]})).is_err());
    }
}
