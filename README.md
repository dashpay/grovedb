# GroveDB
*Hierarchical Authenticated Data Structure with Efficient Secondary Index Queries*
GroveDB is a database system designed specifically for efficient secondary index queries, proofs, speed, and reliability. It was built for use within [Dash Platform](https://dashplatform.readme.io/docs/introduction-what-is-dash-platform). Secondary indices are crucial to any database management system. All previous solutions had certain tradeoffs depending on the problem they were trying to solve. 

## Motivation
Consider a authenticated data structure, like a merkle tree built on a database of say, restaurants. Each restaurant has certain attributes, such as price, and type.

```
struct Restaurant{
	ID uint32;
	name: String;
	kind: String;
	isVegan: bool;
};
```

If we have say, four restaurants, we might normally would commit to them in a merkle tree as follows:

```mermaid
graph TD;
root-->A[" "];
root-->B[" "];
A-->AA["id:0"];
A-->AB["id:1"];
B-->AC["id:2"];
B-->AD["id:3"];
```


Querying by primary key is easy and efficient. If we have a query such as  ```SELECT * WHERE ID <= 2; ```. We can return the appropriate elements, as well as construct an efficient range proof. Querying by a secondary index is not efficient at all. It is likely you have to iterate over the entire structure. Consider the query ``` SELECT * WHERE isVegan=true;```. When sorted by primary key, the vegan restaurant wont be contiguous. Not only will the proof be nontrivial, but so will the time required to find these elements. 

GroveDB is a classic time-space tradeoff. It enables efficient querying on secondary indices by precomputing and committing to them. A subtree of each possible queryable secondary index (up to a cap) is built and committed to into our authenticated data structure. A tree of subtrees; a grove. For the same data, part of the analogous GroveDB structure might look like this:

```mermaid
graph TD;
root-->A["\'Restaurant\'"];
root-->B["..."];
A-->Q["ID"];
A-->W["name"];
A-->E["kind"];
A-->R["isVegan"];
Q-->Z["..."];
W-->X["..."];
E-->C["..."];
R-->Y["id:2"];
R-->U["id:1"];
R-->I["id:0"];
R-->O["id:3"];
```
From here, a query on the secondary index ```isVegan``` would traverse to the subtree built for this secondary index. The items are not necessarily replicated, just references.
## Features
- **efficient secondary index queries** - Built specifically for, and tailored to secondary index queries
- **Proofs** Supports proofs of membership, proofs of non-membership, and range proofs.
- **Run anywhere** being written in Rust, it supports all compile targets. x86, raspberry pis (aarch64), and wasm. There are nodejs bindings as well.

## Architecture
Insertion, deletion work as you might expect, updating the respective subtrees and returning appropriate proofs of membership/nonmembership. On 
### Tree structure(s)
Instead of disjoint authenticated data structures, we opt for one unified one; A hierarchical authenticated data structure, based off of [Database Outsourcing with Hierarchical Authenticated Data Structures](https://ia.cr/2015/351). Elements are the most atomic piece and can be represented in a few ways. They can be items, item references, trees, trees with items, or even trees with item references. an element contains an item, a reference to an object, and a subtree.


The trees are based off our fork of Merk with custom patches applied for better use with groveDB. Merk is unique in fact that it is an AVL tree, so the intermediary nodes also contain a key/value pair. Each node contains a third hash, the ```kv_hash``` in addition to the hashes of its left and right children.The ```kv_hash``` is simply computed as ```kv_hash=H(key,value)```. The node hash is then computed as ```H(kv_hash,left_child_hash,right_child_hash)```. Merk uses Blake2B, and rs-merkle uses SHA256. 

### Storage
RocksDB is a key-value storage built by facebook (based off of LevelDB). We chose it because of its performance. Merk is also built upon RocksDB. 

We have three types of storage, auxillary, metadata, and tree root storage. Auxillary storage is used to store plain key-value data which is not used in consensus.  Metadata has no prefixes. Prefixed things are related to subtrees, and metadata lives at a higher level. Its used to store things outside of the GroveDB usage scope. Tree root storage is used to store subtrees.

A database transaction in GroveDB is a wrapper around the ```OptimisticTransactionDB``` primitive from RocksDB. An optimistic transaction hopes on average there will be few conflicts, only detected at the commit stage. This is as compared to the pessemistic model, which uses a lock. 

## Querying
To query grovedb, a path and a query item has to be supplied.
The path specifies the subtree and the query item determines what nodes are selected from the subtree.

Grovedb currently supports 10 query item types
- Key(key_name)
- Range(start..end)
- RangeInclusive(start..=end)
- RangeFull(..)
- RangeFrom(start..)
- RangeTo(..end)
- RangeToInclusive(..=end)
- RangeAfter(prev..)
- RangeAfterTo(prev..end)
- RangeAfterToInclusive(prev..=end)

This describes a basic query system, select a subtree then select nodes from that subtree. The need might arise to create more complex queries or add restrictions to the result set.
That leads us to the **PathQuery**.

### PathQuery
The PathQuery allows for more complex queries with optional restrictions on the result set i.e limit and offsets. 
```
    PathQuery
        path: [k1, k2, ..]
        sized_query: SizedQuery
            limit: Optional<number>
            offset: Optional<number>
            query: Query
                items: [query_item_1, query_item_2, ...],
                default_subquery_branch: SubqueryBranch
                    subquery_key: Optional<key>
                    subquery_value: Optional<Query>
                conditional_subquery_branches: Map<QueryItem, SubqueryBranch>
                        
```

A path is needed to define the starting context for the query.

### SizedQuery
The sized query determines how the result set would be restricted. It holds optional limit and offset values. 
The limit determines the maximum size of the result set and the offset specifies the number of elements to skip before adding to the result set. 

### Query
The query object is a recursive structure, it specifies how to select nodes from the current subtree and has the option to recursively apply another query to the result set gotten from the previous query. 
- items: a collection of query items that decide what nodes to select from the current context (this builds a result set).  

before describing default and conditional subqueries, we need to define their building block (subquery_branch)

##### subquery_branch
```
    subquery_key: Optional<Key>
    subquery_value: Optional<Query>
```
**Cases**  
subquery_key: true, subquery_value: false  
the node with the subquery key is selected and returned as the result set

subquery_key: false, subquery_value: true  
the query held in subquery_value is applied directly to the subtree, result is returned as result set

subquery_key: true, subquery_value: true  
first the node with the subquery key is selected and set as new context.  
next the subquery value is applied to this new context, result is returned as the result set.

the subquery branch is used on a single node but can be applied to the result set of a previous query with the use of **default_subquery_branch** and **conditional_subquery_branches**

### default_subquery_branch
If this exists, the specified subquery_branch is applied to every node in the result set of the previous query.

### conditional_subquery_branch
Rather than applying a subquery branch to every node in the result set, you might want to apply it to a subset of the result set.  In such cases we make use of a conditional subquery.  
The conditional subquery holds a map QueryItem to SubqueryBranch 
```
    Map<QueryItem, SubqueryBranch>
```
For every node in the result set, we check if there is a query item that matches it, if there is then the associated subquery branch is applied to that node.
Note, once a conditional_subquery has been applied to a node, the default subquery does run on that node.

## Usage
GroveDB is built for use with Dash Platform. See its use in [rs-drive](https://github.com/dashevo/rs-drive) ([example](https://github.com/dashevo/rs-drive-example)). 

We currently also have bindings for nodejs, see [node-grove](https://github.com/dashevo/grovedb/tree/master/node-grove). 

## Building
First, install [rustup](https://www.rust-lang.org/tools/install) using your preferred method. 


Rust nightly is required to build, so ensure you are using the correct version

```rustup install nightly```

Clone the repo and navigate to the main directory

```git clone https://github.com/dashevo/grovedb.git && cd grovedb```

From here we can build 

```cargo build```


## Performance

run with ```cargo test```
|CPU | Time |
|----|-----|
|Raspberry Pi 4 | 2m58.491s|
|R5 1600AF | 33.958s |
|R5 3600 | 25.658s |

