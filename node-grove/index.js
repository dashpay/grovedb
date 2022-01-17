"use strict";

const { promisify } = require("util");

// This file is crated when run `npm run build`. The actual source file that
// exports those functions is ./src/lib.rs
const { groveDbOpen, groveDbGet, groveDbInsert, groveDbProof, groveDbClose, groveDbStartTransaction, groveDbCommitTransaction, groveDbDelete, groveDbInsertIfNotExists } = require("../index.node");

// Convert the DB methods from using callbacks to returning promises
const groveDbGetAsync = promisify(groveDbGet);
const groveDbInsertAsync = promisify(groveDbInsert);
const groveDbInsertIfNotExistsAsync = promisify(groveDbInsertIfNotExists);
const groveDbDeleteAsync = promisify(groveDbDelete);
const groveDbProofAsync = promisify(groveDbProof);
const groveDbCloseAsync = promisify(groveDbClose);
const groveDbStartTransactionAsync = promisify(groveDbStartTransaction);
const groveDbCommitTransactionAsync = promisify(groveDbCommitTransaction);

// Wrapper class for the boxed `Database` for idiomatic JavaScript usage
class GroveDB {
    constructor(db) {
        this.db = db;
    }

    static open(path) {
        const db = groveDbOpen(path);
        return new GroveDB(db);
    }

    /**
     *
     * @param {Buffer[]} path
     * @param {Buffer} key
     * @returns {Promise<Element>}
     */
    async get(path, key, use_transaction) {
        return groveDbGetAsync.call(this.db, path, key, use_transaction);
    }

    /**
     *
     * @param {Buffer[]} path
     * @param {Buffer} key
     * @param {Element} value
     * @returns {Promise<*>}
     */
    async insert(path, key, value, use_transaction) {
        return groveDbInsertAsync.call(this.db, path, key, value, use_transaction);
    }

    async insert_if_not_exists(path, key, value, use_transaction) {
        return groveDbInsertIfNotExistsAsync.call(this.db, path, key, value, use_transaction);
    }

    async put_aux(key, value, use_transaction) {

    }

    async delete_aux(key, use_transaction) {

    }

    async get_aux(key, use_transaction) {

    }

    /**
     * Not implemented in GroveDB yet
     *
     * @returns {Promise<*>}
     */
    async proof(proof_queries) {
        return groveDbProofAsync.call(this.db, proof_queries);
    }

    /**
     * Closes connection to the DB
     *
     * @returns {Promise<void>}
     */
    async close() {
        return groveDbCloseAsync.call(this.db);
    }

    async start_transaction() {
        return groveDbStartTransactionAsync.call(this.db);
    }

    async commit_transaction() {
        return groveDbCommitTransactionAsync.call(this.db);
    }

    async delete(path, key, use_transaction) {
        return groveDbDeleteAsync.call(this.db, path, key, use_transaction);
    }
}

/**
 * @typedef Element
 * @property {string} type - element type. Can be "item", "reference" or "tree"
 * @property {Buffer|Buffer[]} value - element value
 */

module.exports = GroveDB;
