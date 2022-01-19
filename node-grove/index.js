const { promisify } = require('util');

// This file is crated when run `npm run build`. The actual source file that
// exports those functions is ./src/lib.rs
const {
  groveDbOpen,
  groveDbGet,
  groveDbInsert,
  groveDbClose,
  groveDbStartTransaction,
  groveDbCommitTransaction,
  groveDbDelete,
  groveDbInsertIfNotExists,
  groveDbPutAux,
  groveDbDeleteAux,
  groveDbGetAux,
  groveDbGetPathQuery,
} = require('../index.node');

// Convert the DB methods from using callbacks to returning promises
const groveDbGetAsync = promisify(groveDbGet);
const groveDbInsertAsync = promisify(groveDbInsert);
const groveDbInsertIfNotExistsAsync = promisify(groveDbInsertIfNotExists);
const groveDbDeleteAsync = promisify(groveDbDelete);
const groveDbCloseAsync = promisify(groveDbClose);
const groveDbStartTransactionAsync = promisify(groveDbStartTransaction);
const groveDbCommitTransactionAsync = promisify(groveDbCommitTransaction);
const groveDbPutAuxAsync = promisify(groveDbPutAux);
const groveDbDeleteAuxAsync = promisify(groveDbDeleteAux);
const groveDbGetAuxAsync = promisify(groveDbGetAux);
const groveDbGetPathQueryAsync = promisify(groveDbGetPathQuery);

// Wrapper class for the boxed `Database` for idiomatic JavaScript usage
class GroveDB {
  /**
   * @param {string} dbPath
   */
  constructor(dbPath) {
    this.db = groveDbOpen(dbPath);
  }

  /**
   * @param {Buffer[]} path
   * @param {Buffer} key
   * @param {boolean} [useTransaction=false]
   * @returns {Promise<Element>}
   */
  async get(path, key, useTransaction = false) {
    return groveDbGetAsync.call(this.db, path, key, useTransaction);
  }

  /**
   * @param {Buffer[]} path
   * @param {Buffer} key
   * @param {Element} value
   * @param {boolean} [useTransaction=false]
   * @returns {Promise<*>}
   */
  async insert(path, key, value, useTransaction = false) {
    return groveDbInsertAsync.call(this.db, path, key, value, useTransaction);
  }

  /**
   * @param {Buffer[]} path
   * @param {Buffer} key
   * @param {Element} value
   * @param {boolean} [useTransaction=false]
   * @return {Promise<*>}
   */
  async insertIfNotExists(path, key, value, useTransaction = false) {
    return groveDbInsertIfNotExistsAsync.call(this.db, path, key, value, useTransaction);
  }

  /**
   *
   * @param {Buffer[]} path
   * @param {Buffer} key
   * @param {boolean} [useTransaction=false]
   * @return {Promise<*>}
   */
  async delete(path, key, useTransaction = false) {
    return groveDbDeleteAsync.call(this.db, path, key, useTransaction);
  }

  /**
   * Closes connection to the DB
   *
   * @returns {Promise<void>}
   */
  async close() {
    return groveDbCloseAsync.call(this.db);
  }

  /**
   * Start a transaction with isolated scope
   *
   * Write operations will be allowed only for the transaction
   * until it's committed
   *
   * @return {Promise<void>}
   */
  async startTransaction() {
    return groveDbStartTransactionAsync.call(this.db);
  }

  /**
   * Commit transaction
   *
   * Transaction should be started before
   *
   * @return {Promise<void>}
   */
  async commitTransaction() {
    return groveDbCommitTransactionAsync.call(this.db);
  }

  /**
   * Put auxiliary data
   *
   * @param {Buffer} key
   * @param {Buffer} value
   * @param {boolean} [useTransaction=false]
   * @return {Promise<*>}
   */
  async putAux(key, value, useTransaction = false) {
    return groveDbPutAuxAsync.call(this.db, key, value, useTransaction);
  }

  /**
   * Delete auxiliary data
   *
   * @param {Buffer} key
   * @param {boolean} [useTransaction=false]
   * @return {Promise<*>}
   */
  async deleteAux(key, useTransaction = false) {
    return groveDbDeleteAuxAsync.call(this.db, key, useTransaction);
  }

  /**
   * Get auxiliary data
   * 
   * @param {Buffer} key
   * @param {boolean} [useTransaction=false]
   * @return {Promise<Buffer>}
   */
  async getAux(key, useTransaction = false) {
    return groveDbGetAuxAsync.call(this.db, key, useTransaction);
  }

  /**
   * Get data using query.
   * 
   * @param {PathQuery}
   * @param {boolean} [useTransaction=false]
   * @return {Promise<*>}
   */
  async getPathQuery(query, useTransaction = false) {
    return groveDbGetPathQueryAsync.call(this.db, query, useTransaction);
  }
}

/**
 * @typedef Element
 * @property {string} type - element type. Can be "item", "reference" or "tree"
 * @property {Buffer|Buffer[]} value - element value
 */

/**
 * @typedef PathQuery
 * @property {Buffer[]} path
 * @property {SizedQuery} query
 * @property {Buffer|null} subqueryKey
 * @property {Query|null} subquery
 */

/**
 * @typedef SizedQuery
 * @property {Query} query
 * @property {Number|null} limit
 * @property {Number|null} offset
 * @property {boolean} leftToRight
 */

/**
 * @typedef Query
 * @property {Array} items
 */

module.exports = GroveDB;
