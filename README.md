# GroveDB


GroveDB is a hierarchical authenticated data structure built for internal use. The construction is based on [Database Outsourcing with Hierarchical Authenticated Data Structures](https://eprint.iacr.org/2015/351.pdf).

Instead of several, separate autheticated data structures (ADS), we opted to build a hierarchy of them; a tree of sub-trees. This is where the name GroveDB comes from. A subtree root hash is a leaf of an upper level tree. 

# Building
requires rust nightly. Build with ```cargo run```
