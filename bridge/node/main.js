#!/usr/bin/env node
const { main } = require("./index.js")
;(async()=>await main().catch(console.error))()