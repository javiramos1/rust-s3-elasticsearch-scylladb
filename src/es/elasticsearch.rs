use crate::data::{model::IndexNode, rest_api::SearchRequest};
use elasticsearch::{
    auth::Credentials,
    http::transport::{SingleNodeConnectionPool, TransportBuilder},
    indices::{IndicesCreateParts, IndicesDeleteParts, IndicesExistsParts},
    BulkOperation, BulkParts, Elasticsearch, SearchParts,
};
use http::StatusCode;
use serde_json::Value;
use std::io::{Error as StdError, ErrorKind};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{AcquireError, OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};
use url::Url;

const DELETE_INDEX: bool = false;
const MAX_ES_RESULT: i64 = 1000;

pub struct ElasticSearchService {
    svc: Arc<Elasticsearch>,
    shards: usize,
    refresh_interval: String,
    source_enabled: bool,
    parallelism: usize,
    batch_size: usize,
    index_name: String,
}

impl ElasticSearchService {
    pub async fn new(
        url_str: String,
        parallelism: usize,
        num_shards: usize,
        batch_size: usize,
        index: String,
        refresh_interval: String,
        source_enabled: bool,
        username: Option<String>,
        password: Option<String>,
    ) -> Self {
        info!(
            "ElasticSearchService: Creating ElasticSearchService using {} for user {:?}",
            url_str, username
        );
        info!(
            "ElasticSearchService: ElasticSearchService index {}, shards {}, parallelism {}, batch size {}",
            index, num_shards, parallelism, batch_size
        );

        let url = Url::parse(url_str.as_str()).expect("Invalid URL");
        let conn_pool = SingleNodeConnectionPool::new(url);

        let mut builder = TransportBuilder::new(conn_pool);
        match username {
            Some(user) => {
                let credentials = Credentials::Basic(user, password.expect("Missing Password"));
                info!("ElasticSearchService: Adding credentials {:?}", credentials);
                builder = builder.auth(credentials);
            }
            None => (),
        };

        let transport = builder.build().expect("ElasticSearch config Error");
        let client = Elasticsearch::new(transport);
        info!("ElasticSearchService: ElasticSearchService Created! Creating Index if not exist...");

        let svc = ElasticSearchService {
            svc: Arc::new(client),
            shards: num_shards,
            parallelism,
            batch_size,
            index_name: index.clone(),
            refresh_interval: refresh_interval.clone(),
            source_enabled,
        };

        svc.create_index_if_not_exists(&index, DELETE_INDEX)
            .await
            .expect("Error creating Index");

        info!("ElasticSearchService: Index processed!");

        svc
    }

    pub async fn search(
        &self,
        request: SearchRequest,
    ) -> Result<Vec<String>, Box<dyn std::error::Error + Sync + Send>> {
        let q = if request.tags.is_some() {
            let tags = request.tags.unwrap();
            json!(
                {
                    "query": {
                        "nested": {
                            "path": "tags",
                            "query": {
                                "bool": {
                                    "must": [
                                        {
                                            "match": {
                                                "tags.type": tags.tag_type
                                            }
                                        },
                                        {
                                            "wildcard": {
                                                "tags.value": tags.value
                                            }
                                        }
                                    ]
                                }
                            }
                        }
                    }
                }
            )
        } else {
            json!(
                {
                    "query": {
                        "query_string": {
                            "query": request.query
                        }
                    }
                }
            )
        };

        info!("ElasticSearchService: search query: {}", q);

        let response = self
            .svc
            .search(SearchParts::Index(&[self.index_name.as_str()]))
            .body(q)
            .size(MAX_ES_RESULT)
            .send()
            .await?;

        debug!(
            "ElasticSearchService: search query raw response: {:?}",
            response
        );
        let json: Value = response.error_for_status_code()?.json().await?;
        debug!(
            "ElasticSearchService: search query raw response json: {:?}",
            json
        );

        let ids: Vec<String> = json["hits"]["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| serde_json::from_value(i["_id"].clone()).unwrap())
            .collect();

        debug!("ElasticSearchService: search query response: {:?}", ids);
        Ok(ids)
    }

    pub async fn index_nodes(
        &self,
        nodes: Vec<IndexNode>,
    ) -> Result<(), Box<dyn std::error::Error + Sync + Send>> {
        info!("ElasticSearchService: index_nodes: Starting...");
        let now = Instant::now();

        let sem = Arc::new(Semaphore::new(self.parallelism));

        let mut handlers: Vec<JoinHandle<_>> = Vec::new();
        let mut batch: Vec<IndexNode> = Vec::new();
        let mut i = 0;
        let total_len = nodes.len();
        let mut b = 1;
        let mut current_batch = 0;
        for n in nodes {
            batch.push(n);
            if current_batch >= self.batch_size || current_batch >= total_len - 1 {
                debug!("ElasticSearchService: index_nodes: Processing batch {} with {} nodes. Processed: {}/{}", b, batch.len(), i, total_len);
                let permit = sem.clone().acquire_owned().await;
                let client = self.svc.clone();
                handlers.push(tokio::task::spawn(index(
                    client,
                    batch,
                    permit,
                    self.index_name.clone(),
                )));

                batch = vec![];
                b += 1;
                current_batch = 0;
            }
            i += 1;
            current_batch += 1;
        }

        info!(
            "ElasticSearchService: index_nodes: Waiting for {} tasks to complete...",
            i
        );

        let mut error_count = 0;
        for thread in handlers {
            match thread.await? {
                Err(e) => {
                    error!(
                        "ElasticSearchService: index_nodes:: Error Indexing Nodes. {:?}",
                        e
                    );
                    error_count += 1;
                }
                Ok(r) => debug!("ElasticSearchService: index_nodes:: Index Result: {:?}", r),
            };
        }

        let elapsed = now.elapsed();
        info!("ElasticSearchService: index_nodes: {} index nodes tasks completed. ERRORS: {}. Took: {:.2?}", i, error_count, elapsed);

        Ok(())
    }

    async fn create_index_if_not_exists(
        &self,
        index: &str,
        delete: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        info!(
            "ElasticSearchService: create_index_if_not_exists {}: Starting, delete? {}",
            index, delete
        );

        let client = &self.svc;
        let exists = client
            .indices()
            .exists(IndicesExistsParts::Index(&[index]))
            .send()
            .await?;

        debug!("exists? {:?}", exists);

        if exists.status_code().is_success() && delete {
            info!("Deleting Index...");
            client
                .indices()
                .delete(IndicesDeleteParts::Index(&[index]))
                .send()
                .await?;
        }

        if exists.status_code() == StatusCode::NOT_FOUND || delete {
            info!("Creating Index...");
            let response = client
                .indices()
                .create(IndicesCreateParts::Index(index))
                .body(json!(
                  {
                    "settings": {
                      "index.number_of_shards": self.shards,
                      "index.number_of_replicas": 0,
                      "index.refresh_interval": self.refresh_interval           
                    },
                    "mappings": {
                      "_source": {
                        "enabled": self.source_enabled
                      },
                      "dynamic": "strict",
                      "properties": {
                        "type": {
                          "analyzer": "english",
                          "type": "text"
                        },
                        "name": {
                          "analyzer": "english",
                          "type": "text",
                          "fields": {
                            "keyword": {
                              "type": "keyword"
                            }
                          }
                        },
                        "uuid": {
                            "type": "text",
                            "index": "false"
                        },
                        "tags": {
                          "type": "nested",
                          "properties": {
                            "type": {
                              "analyzer": "english",
                              "type": "text"
                            },
                            "value": {
                              "analyzer": "english",
                              "type": "text",
                              "fields": {
                                "keyword": {
                                  "type": "keyword"
                                }
                              }
                            }
                          }
                        }
                      }
                    }
                  }
                ))
                .send()
                .await?;

            if !response.status_code().is_success() {
                error!("Error while creating index: {:?}", response.text().await);
                return Err(Box::new(StdError::new(
                    ErrorKind::Other,
                    "Error Creating index!",
                )));
            }
        }

        Ok(())
    }
}

async fn index(
    client: Arc<Elasticsearch>,
    nodes: Vec<IndexNode>,
    permit: Result<OwnedSemaphorePermit, AcquireError>,
    index_name: String,
) -> Result<(), Box<dyn std::error::Error + Sync + Send>> {
    debug!("ElasticSearchService: index_nodes: index {}", nodes.len());
    let body: Vec<BulkOperation<_>> = nodes
        .iter()
        .map(|n| {
            let id = n.uuid.to_string();
            BulkOperation::index(n).id(&id).into()
        })
        .collect();
    debug!(
        "ElasticSearchService: index_nodes: Bulk Operation Created, size: {}. Indexing...",
        body.len()
    );
    let response = client
        .bulk(BulkParts::Index(&index_name))
        .body(body)
        .send()
        .await?;
    let json: Value = response.json().await?;
    debug!("ElasticSearchService: index_nodes: Response: {:?}", json);
    if json["errors"].as_bool().unwrap_or(false) {
        warn!("Errors found during indexing...");
        let failed_len = json["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| {
                let result = !&v["error"].is_null();
                if result {
                    warn!("ES ERROR: {:?}", &v["error"]);
                }
                result
            })
            .count();

        debug!("failed_len {:?}", failed_len);
        // TODO: retry failures
        if failed_len > 0 {
            warn!("Errors whilst indexing. Failures: {}", failed_len);
            if failed_len > nodes.len() / 2 {
                // fail if more than half failed
                error!(
                    "Indexing ERROR. Failures: {} of {}",
                    failed_len,
                    nodes.len()
                );
                return Err(Box::new(StdError::new(
                    ErrorKind::Other,
                    "Error Indexing nodes!",
                )));
            }
        }
    }
    debug!("ElasticSearchService: index_nodes: Completed!");
    let _permit = permit;
    Ok(())
}
