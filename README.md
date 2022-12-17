# Rust Real World Example: S3 + ElasticSearch + ScyllaDB

This repo contains a real world example of a full blown application which showcases the following capabilities:
- Creating ultra fast REST APIs using [Actix Web Framework](https://actix.rs/)
- Reading and parsing Big Data files from **AWS S3** including credentials management.
- Indexing massive amounts of data into [ElasticSearch](https://www.elastic.co/elasticsearch/) using the bulk api and multiple threads.
- Fast data ingestion from S3 into [ScyllaDB](https://www.scylladb.com/) (Cassandra compatible).
- **Graph** data modeling in a **NoSQL** database for super fast graph traversals of the nodes.
- Querying ElasticSearch.
- Writing highly async concurrent applications using [Tokio](https://tokio.rs/)
- Best cloud native practices: multi-stage docker build, kubernetes, microservices, etc.

## Use Case

This repo presents the following use case:

- You have hierarchical or graph data stored in a data lake (S3).
- You need to ingest it with high throughput and speed into Cassandra (ScyllaDB).
- You need to index the data to perform full text search including suggestions (ElasticSearch).
- You need to traverse or search the graph very fast.

The application has a REST API to trigger the ingest process and also to get a node and traverse a node.

The ingestion process will read files from a S3 bucket, process them and store them into ScyllaDB and index them into ElasticSearch.

## Goals

This project has a Makefile with the following goals:

- `make db`: Starts ElasticSearch and ScyllaDB using docker-compose. Make sure it is installed in your machine.
- `make stop`: Stops ElasticSearch and ScyllaDB.
- `make bld`: Builds the  application.
- `make run`: Runs application.
- `make build`: Builds docker image.
- `make push`: Pushes docker image.

## REST API

### Data Ingestion

#### POST /ingest

Triggers the ingestion process from one or more files:

Body:

```
{
    "ingestion_id": "test",
    "files": [
        "s3://rust-s3-scylladb/data_example.json"
    ]
}
```

- `ingestion_id` is a unique ID that you can use to identify a single ingestion.
- `files`: contains a list of files to be processed.

The idea is that you will deploy multiple replicas of this service to run in parallel.
I have included a small [Python Script](/job/ingestion_job.py) that you can use to read from a bucket and call this REST API to distribute the load.

#### GET /node/{id}

Get a specific node by ID.

Query Parameters:

- `get_tags`: If `true` it will also return the tags.
- `get_relations`: If `true` it will also return the relations.

#### GET /traversal/{id}

Traverse the tree from a specific node.

Query Parameters:

- `max_depth`: The maximum `depth` you want to explore.
- `direction`: The direction you want to explore. Select `OUT` for outbound relations leaving the node. Select `IN` for inbound relations coming into the node. In a tree, `IN` would be to go from children to parent and `OUT` to go from parent to children.
- `relation_type`: Besides direction, you can add an additional filter by relation type, use this to filter for specific relations.

#### GET /search/

Search nodes and tags in the graph using ElasticSearch(ES).

Body:

To search any field:

```
{
    "query": "children1*",
    "return_details": true,
    "return_tags": true,
    "return_relations": true
}
```

By default it only returns the IDs.

- `query` contains the ElasticSearch query. Under the hood it uses ES `query_string` method.
- `return_details`: if `true` it will also return the details which are grabbed from ScyllaDB.
- `return_tags`: if `true` it also returns the tags.
- `return_relations`: if `true` it also returns the relations.

If you want to search a specific vale you can enter the field `:` and the query. For example `name:children1`.

### Input Data

You can find an example data [here](/data/data_example.json). 

Please upload it to your S3 bucket.

## Env Vars

- `PORT`: Port to run the REST API.
- `REGION`: AWS Region where the bucket is located.
- `RUST_LOG`: Log level
- `AWS_ACCESS_KEY_ID`: AWS Access Key used to read files from S3. Make sure it has permission to list and read objects.
- `AWS_SECRET_ACCESS_KEY`: AWS Secret.
- `DB_URL`: ScyllaDB URL, for example `localhost:9042`.
- `DB_DC`: ScyllaDB DC, for example `datacenter1`.
- `PARALLEL_FILES`: Number of files to process in parallel regardless of the HTTP request. Reduce this for backpressure.
- `DB_PARALLELISM`: Parallelism for the database, number of threads that will be running inserts in parallel. ScyllaDB can support hundreds or even thousands of them.
- `SCHEMA_FILE`: Location of the schema, for example `schema/ddl.sql`.
- `ES_ENABLED`: If `true` ElasticSearch is enabled.
- `ES_BATCH_SIZE`: Batch size for ES ingestion.
- `ES_URL`: ElasticSearch URL, for example `http://localhost:9200`
- `ES_NUM_SHARDS`: Number of shards for the index.
- `ES_INDEX`: Index name.
- `ES_USER`: ElasticSearch username credentials.
- `ES_PASSWORD`: ElasticSearch password.
- `ES_PARALLELISM`: number of parallel bulk requests for ES ingestion.
- `ES_REFRESH_INTERVAL`: Index refresh interval.
- `ES_SOURCE_ENABLED`: If `true` source in ES will be enabled and the original document will be stored, this requires more space but it will provide the original value and enable extra features.

## Data Model

This repo provides an example on how to model graph data using a NoSQL database. As you know, in NoSQL you model your data around your queries. This model targets certain queries that may not fit your usage patterns, this model may to be ideal in several situations and you should change it accordingly to your needs.

```
CREATE TABLE IF NOT EXISTS graph.nodes (
   id uuid,
   ingestion_id text,
   name text,
   url text,
   item_type text,
   direction text,
   relation text,
   relates_to text,
   tags list<frozen<tuple<text,text>>>,
   PRIMARY KEY (id, direction, relation, relates_to)
) WITH comment = 'Nodes Table' AND caching = {'enabled': 'true'} 
    AND compression = {'sstable_compression': 'LZ4Compressor'}
    AND CLUSTERING ORDER BY (direction ASC, relation ASC, relates_to DESC);
```

You can find the DDL [here](/schema/ddl.sql).

### ElasticSearch Index Mapping

These are the Index settings:

```
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
```

