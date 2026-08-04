const { spawnSync } = require('node:child_process')
const path = require('node:path')

const test = require('ava').default

const cliPath = path.resolve(__dirname, '../main.js')

const runCli = (arguments_) =>
  spawnSync(process.execPath, [cliPath, ...arguments_], {
    encoding: 'utf8',
    windowsHide: true,
  })

test('prints the version and exits successfully', (t) => {
  // Given the native bridge built by the package test script
  // When the Node entry point receives the version flag
  const result = runCli(['--version'])

  // Then it returns a semantic version on stdout without an error
  t.is(result.status, 0)
  t.is(result.signal, null)
  t.is(result.stderr, '')
  t.regex(result.stdout, /^changepacks \d+\.\d+\.\d+\r?\n$/)
})

test('rejects an unknown command with usage information', (t) => {
  // Given the native bridge built by the package test script
  // When the Node entry point receives an unknown command
  const result = runCli(['definitely-not-a-command'])

  // Then Clap reports a usage error on stderr
  t.is(result.status, 2)
  t.is(result.signal, null)
  t.is(result.stdout, '')
  t.regex(result.stderr, /^error: [^\r\n]+\r?\n/)
  t.regex(result.stderr, /\r?\nUsage: \S+ \[OPTIONS\] \[COMMAND\]\r?\n/)
})
