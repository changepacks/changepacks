#!/usr/bin/env node
const { main } = require('./index.js')
;(async () => {
  try {
    await main()
  } catch (error) {
    console.error(error)
    process.exitCode = 1
  }
})()
