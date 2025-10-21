#!/usr/bin/env node
const { main } = require("./dist/index.js")
;(async()=>await main().catch(console.error))()