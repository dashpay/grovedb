"use strict";

const { promisify } = require("util");

const { groveDbOpen, groveDbGet, groveDbInsert, groveDbProof } = require("./index.node");

// Convert the DB methods from using callbacks to returning promises
const groveDbGetAsync = promisify(groveDbGet);

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
     * @returns {*}
     */
    get(path, key) {
        return groveDbGetAsync.call(this.db, path, key);
    }

    insert() {}

    proof() {}
}

module.exports = GroveDB;
