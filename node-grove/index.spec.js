const GroveDB = require('./index.js');
const rimraf = require('rimraf');
const { promisify } = require("util");
const removeTestDataFiles = promisify(rimraf);

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

    it('should store and retrieve a value', async function createGroveDb() {
        console.log('Wow, so much test!');
    });
});