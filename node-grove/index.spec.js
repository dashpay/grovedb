const GroveDB = require('./index.js');
const rimraf = require('rimraf');
const { promisify } = require("util");
const removeTestDataFiles = promisify(rimraf);
const { expect } = require('chai');

const testDataPath = './test_data';

describe('GroveDB', () => {
    let groveDb;

    beforeEach(() => {
        groveDb = GroveDB.open(testDataPath);
    });

    afterEach(async () => {
        await groveDb.close();
        await removeTestDataFiles(testDataPath);
    });

    it('should store and retrieve a value', async () => {
        const tree_key = Buffer.from("test_tree");

        const item_key = Buffer.from("test_key");
        const item_value = Buffer.from("very nice test value");

        const root_tree_path = [];
        const item_tree_path = [tree_key];

        // Making a subtree to insert items into
        await groveDb.insert(
            root_tree_path,
            tree_key,
            { type: "tree", value: Buffer.alloc(32) },
            false
        );

        // Inserting an item into the subtree
        await groveDb.insert(
            item_tree_path,
            item_key,
            { type: "item", value: item_value },
            false
        );

        const element = await groveDb.get(item_tree_path, item_key, false);

        expect(element.type).to.be.equal("item");
        expect(element.value.toString()).to.be.equal("very nice test value");
    });

    it('should work with transactions', async () => {
        const tree_key = Buffer.from("test_tree");

        const item_key = Buffer.from("test_key");
        const item_value = Buffer.from("very nice test value");

        const root_tree_path = [];
        const item_tree_path = [tree_key];

        // Making a subtree to insert items into
        await groveDb.insert(
            root_tree_path,
            tree_key,
            { type: "tree", value: Buffer.alloc(32) },
            false
        );

        await groveDb.start_transaction();

        // Inserting an item into the subtree
        await groveDb.insert(
            item_tree_path,
            item_key,
            { type: "item", value: item_value },
            true
        );

        // Inserted value is not yet commited, but can be retrieved by `get`
        // with `use_transaction` flag.
        const element_in_transaction = await groveDb.get(item_tree_path, item_key, true);
        expect(element_in_transaction.type).to.be.equal("item");
        expect(element_in_transaction.value.toString()).to.be.equal("very nice test value");

        // ... and using `get` without the flag should return no value
        try {
            await groveDb.get(item_tree_path, item_key, false);
            expect.fail("Expected to throw an error")
        } catch (e) {
            expect(e.message).to.be.equal("invalid path: key not found in Merk");
        }

        await groveDb.commit_transaction();

        // When commited, the value should be accessible without running transaction
        const element_ = await groveDb.get(item_tree_path, item_key, false);
        expect(element_.type).to.be.equal("item");
        expect(element_.value.toString()).to.be.equal("very nice test value");
    });

    describe('#insert', () => {
        it('should be able to insert a tree', async () => {
            await groveDb.insert([], Buffer.from("test_tree"), { type: "tree", value: Buffer.alloc(32) }, false)
        });

        it('should throw when trying to insert non-existent element type', async () => {
            const path = [];
            const key = Buffer.from("test_key");

            try {
                await groveDb.insert(path, key, { type: "not_a_tree", value: Buffer.alloc(32) }, false)
                expect.fail("Expected to throw en error");
            } catch (e) {
                expect(e.message).to.be.equal("Unexpected element type not_a_tree");
            }
        });

        it('should throw when trying to insert a tree that is not 32 bytes', async () => {
            const path = [];
            const key = Buffer.from("test_key");

            try {
                await groveDb.insert(path, key, { type: "tree", value: Buffer.alloc(1) }, false)
                expect.fail("Expected to throw en error");
            } catch (e) {
                expect(e.message).to.be.equal("Tree buffer is expected to be 32 bytes long, but got 1");
            }
        });
    })
});
