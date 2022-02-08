# GroveDB


GroveDB is a hierarchical authenticated data structure built for internal use. The construction is based on [Database Outsourcing with Hierarchical Authenticated Data Structures](https://eprint.iacr.org/2015/351.pdf).

Instead of several, separate autheticated data structures (ADS), we opted to build a hierarchy of them; a tree of sub-trees. This is where the name GroveDB comes from. A subtree root hash is a leaf of an upper level tree. 

# Building
First, install [rustup](https://www.rust-lang.org/tools/install) using your preferred method. 


Rust nightly is required to build, so ensure you are using the correct version

```rustup install nightly```

Clone the repo and navigate to the main directory

```git clone https://github.com/dashevo/grovedb.git && cd grovedb```

From here we can build 

```cargo build```

It may take some time to build initially. We can also run tests with

```cargo test```
