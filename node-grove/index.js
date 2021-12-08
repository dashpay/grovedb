"use strict";

const { promisify } = require("util");

const { groveDbOpen, groveDbGet } = require("./index.node");

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

    // Wrap each method with a delegate to `this.db`
    // This could be node in several other ways, for example binding assignment
    // in the constructor
    get(name) {
        return groveDbGetAsync.call(this.db, name);
    }
}

module.exports = GroveDB;