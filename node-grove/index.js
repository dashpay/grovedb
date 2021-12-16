"use strict";

const { promisify } = require("util");

// This file is crated when run `npm run build`. The actual source file that
// exports those functions is ./src/lib.rs
const { groveDbOpen, groveDbGet, groveDbInsert, groveDbProof, groveDbClose } = require("./index.node");

// Convert the DB methods from using callbacks to returning promises
const groveDbGetAsync = promisify(groveDbGet);
const groveDbInsertAsync = promisify(groveDbInsert);
const groveDbProofAsync = promisify(groveDbProof);
const groveDbCloseAsync = promisify(groveDbClose);

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
    async get(path, key) {
        return groveDbGetAsync.call(this.db, path, key);
    }

    /**
     *
     * @param {Buffer[]} path
     * @param {Buffer} key
     * @param {Element} value
     * @returns {Promise<*>}
     */
    async insert(path, key, value) {
        return groveDbInsertAsync.call(this.db, path, key, value);
    }

    /**
     * Not implemented in GroveDB yet
     *
     * @returns {Promise<*>}
     */
    async proof() {
        return groveDbProofAsync.call(this.db);
    }

    /**
     * Closes connection to the DB
     *
     * @returns {Promise<void>}
     */
    async close() {
        return groveDbCloseAsync.call(this.db);
    }
}

/**
 * @typedef Element
 * @property {string} type - element type. Can be "item", "reference" or "tree"
 * @property {Buffer|Buffer[]} value - element value
 */

module.exports = GroveDB;
