"use strict";

const { promisify } = require("util");

const { groveDbOpen, groveDbGet, groveDbInsert, groveDbProof } = require("./index.node");

// Convert the DB methods from using callbacks to returning promises
const groveDbGetAsync = promisify(groveDbGet);
const groveDbInsertAsync = promisify(groveDbInsert);
const groveDbOpenAsync = promisify(groveDbOpen);
const groveDbProofAsync = promisify(groveDbProof);

// Wrapper class for the boxed `Database` for idiomatic JavaScript usage
class GroveDB {
    constructor(db) {
        this.db = db;
    }

    static async open(path) {
        const db = await groveDbOpenAsync(path);
        return new GroveDB(db);
    }

    /**
     *
     * @param {Buffer[]} path
     * @param {Buffer} key
     * @returns {*}
     */
    async get(path, key) {
        return groveDbGetAsync.call(this.db, path, key);
    }

    async insert() {
        return groveDbInsertAsync.call(this.db);
    }

    async proof() {
        return groveDbProofAsync.call(this.db);
    }
}

module.exports = GroveDB;
