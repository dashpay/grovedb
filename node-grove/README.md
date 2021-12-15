# node-grove

[![Rayon crate](https://img.shields.io/crates/v/rs_merkle.svg)](https://crates.io/crates/rs_merkle)
[![Rayon documentation](https://docs.rs/rs_merkle/badge.svg)](https://docs.rs/rs_merkle)
[![Build and test](https://github.com/antouhou/rs-merkle/actions/workflows/test.yml/badge.svg?branch=master)](https://github.com/antouhou/rs-merkle/actions)

`node-grove` is a groveDB binding for node.js

`node-grove` is [available on npm](https://npmjs.org/node-grove)

## Usage

Add the module to your project with `npm install node-grove`.

## Example

```javascript
const GroveDB = require('node-grove');

async function main() {
    const groveDb = GroveDB.open('./test.db');
    
    await groveDb.insert(path, key, value);
    
    const result = await groveDb.get(path, key);
    
    // Don't forget to close connection when you no longer need it
    await groveDb.close();
}
```

## Building and testing

Run `npm run build` to build the package, `npm test` to test it.

## How it works

The main file that is used form the node.js side is `index.js`. It contains
class names `GroveDb`. The actual functions this class makes calls to are
stored in the `./src/lib.rs`. When building the project, it is compiled to 
a file called `index.node`, that is imported into the `index.js` file.

Please note that the binding itself contains a lot of code. This is due to 
the fact that GroveDB is not thread-safe, and needs to live in its own thread.
It communicates with the main binding thread through messages.

## Contributing

Everyone is welcome to contribute in any way or form! For further details,
please read [CONTRIBUTING.md](./CONTRIBUTING.md) (Which doesn't really exist in
this repo lol)

## Authors
- [Anton Suprunchuk](https://github.com/antouhou) - [Website](https://antouhou.com)

Also, see the list of contributors who participated in this project.

## License

This project is licensed under the MIT License - see the
[LICENSE.md](./LICENSE.md) file for details