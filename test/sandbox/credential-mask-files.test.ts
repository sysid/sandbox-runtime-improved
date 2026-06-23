import { describe, test, expect, beforeAll, afterAll } from 'bun:test'
import { spawn, spawnSync } from 'node:child_process'
import {
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs'
import {
  createServer as createHttpServer,
  type IncomingHttpHeaders,
} from 'node:http'
import type { Server, AddressInfo } from 'node:net'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import {
  MaskedFileStore,
  buildMaskedFileBinds,
  MASKED_FILE_STORE_PREFIX,
} from '../../src/sandbox/credential-mask-files.js'
import {
  SentinelRegistry,
  SENTINEL_PREFIX,
} from '../../src/sandbox/credential-sentinel.js'
import { SandboxManager } from '../../src/sandbox/sandbox-manager.js'
import type { SandboxRuntimeConfig } from '../../src/sandbox/sandbox-config.js'
import { isLinux } from '../helpers/platform.js'

/**
 * Unit tests for the fake-file store and bind builder. Platform-agnostic;
 * these touch only the host filesystem and the sentinel registry.
 */

const FIXTURE_DIR = join(tmpdir(), 'srt-credmask-fixture-' + Date.now())
const TOKEN_FILE = join(FIXTURE_DIR, 'gh-token')
const TOKEN_CONTENT = 'ghp_realsecret_abcdef0123456789'
const SUBDIR = join(FIXTURE_DIR, 'aws-dir')

beforeAll(() => {
  mkdirSync(SUBDIR, { recursive: true })
  writeFileSync(TOKEN_FILE, TOKEN_CONTENT)
})

afterAll(() => {
  rmSync(FIXTURE_DIR, { recursive: true, force: true })
})

describe('MaskedFileStore', () => {
  test('lazily creates a temp dir under os.tmpdir()', () => {
    const store = new MaskedFileStore()
    expect(store.dirPath).toBeUndefined()
    const fake = store.write('k', 'sentinel')
    expect(store.dirPath).toBeDefined()
    expect(store.dirPath!.startsWith(tmpdir())).toBe(true)
    expect(store.dirPath!).toContain(MASKED_FILE_STORE_PREFIX)
    expect(readFileSync(fake, 'utf8')).toBe('sentinel')
    store.dispose()
  })

  test('store dir is outside the default sandbox-writable temp', () => {
    // The default writable temp inside the sandbox is /tmp/claude (see
    // getDefaultWritePaths). The fake dir must NOT be under it, or the
    // sandboxed process could tamper with the sentinel source.
    const store = new MaskedFileStore()
    store.write('k', 'x')
    expect(store.dirPath!.startsWith('/tmp/claude/')).toBe(false)
    expect(store.dirPath!.startsWith('/private/tmp/claude/')).toBe(false)
    store.dispose()
  })

  test('write is idempotent on key — same fake path, content overwritten', () => {
    const store = new MaskedFileStore()
    const a = store.write('file:/x', 'first')
    const b = store.write('file:/x', 'second')
    expect(b).toBe(a)
    expect(readFileSync(a, 'utf8')).toBe('second')
    expect(readdirSync(store.dirPath!)).toHaveLength(1)
    store.dispose()
  })

  test('dispose removes the temp dir and is idempotent', () => {
    const store = new MaskedFileStore()
    store.write('k', 'x')
    const dir = store.dirPath!
    expect(existsSync(dir)).toBe(true)
    store.dispose()
    expect(existsSync(dir)).toBe(false)
    expect(store.dirPath).toBeUndefined()
    store.dispose() // no-op, no throw
  })
})

describe('buildMaskedFileBinds', () => {
  test('registers a sentinel keyed on file path and writes it to a fake', () => {
    const reg = new SentinelRegistry()
    const store = new MaskedFileStore()
    const binds = buildMaskedFileBinds(
      [{ path: TOKEN_FILE, mode: 'mask' }],
      ['api.github.com'],
      reg,
      store,
    )
    expect(binds).toHaveLength(1)
    expect(binds[0]!.realPath).toBe(TOKEN_FILE)
    const fakeContent = readFileSync(binds[0]!.fakePath, 'utf8')
    expect(fakeContent.startsWith(SENTINEL_PREFIX)).toBe(true)
    // The fake holds the sentinel, never the real bytes.
    expect(fakeContent).not.toContain(TOKEN_CONTENT)
    // The registry maps that sentinel back to the real file content.
    expect(reg.lookupReal(fakeContent)).toBe(TOKEN_CONTENT)
    store.dispose()
  })

  test('a file sentinel only substitutes at its own injectHosts', () => {
    const reg = new SentinelRegistry()
    const store = new MaskedFileStore()
    const binds = buildMaskedFileBinds(
      [{ path: TOKEN_FILE, mode: 'mask', injectHosts: ['api.github.com'] }],
      ['api.github.com', 'evil.example.com'],
      reg,
      store,
    )
    const sentinel = readFileSync(binds[0]!.fakePath, 'utf8')
    const eq = (h: string, p: string) => h === p

    const toGh = { authorization: `Bearer ${sentinel}` }
    reg.substituteInHeaders(toGh, 'api.github.com', eq)
    expect(toGh.authorization).toBe(`Bearer ${TOKEN_CONTENT}`)

    const toEvil = { authorization: `Bearer ${sentinel}` }
    reg.substituteInHeaders(toEvil, 'evil.example.com', eq)
    expect(toEvil.authorization).toBe(`Bearer ${sentinel}`)
    store.dispose()
  })

  test('absent injectHosts → defaults to allowedDomains', () => {
    const reg = new SentinelRegistry()
    const store = new MaskedFileStore()
    const binds = buildMaskedFileBinds(
      [{ path: TOKEN_FILE, mode: 'mask' }],
      ['fallback.example.com'],
      reg,
      store,
    )
    const sentinel = readFileSync(binds[0]!.fakePath, 'utf8')
    const eq = (h: string, p: string) => h === p
    const headers = { authorization: sentinel }
    reg.substituteInHeaders(headers, 'fallback.example.com', eq)
    expect(headers.authorization).toBe(TOKEN_CONTENT)
    store.dispose()
  })

  test('skips a masked file that does not exist on the host', () => {
    const reg = new SentinelRegistry()
    const store = new MaskedFileStore()
    const binds = buildMaskedFileBinds(
      [{ path: join(FIXTURE_DIR, 'no-such-file'), mode: 'mask' }],
      [],
      reg,
      store,
    )
    expect(binds).toHaveLength(0)
    expect(reg.size).toBe(0)
    // No fake was written → store dir was never created.
    expect(store.dirPath).toBeUndefined()
    store.dispose()
  })

  test('skips a masked entry that resolves to a directory', () => {
    const reg = new SentinelRegistry()
    const store = new MaskedFileStore()
    const binds = buildMaskedFileBinds(
      [{ path: SUBDIR, mode: 'mask' }],
      [],
      reg,
      store,
    )
    expect(binds).toHaveLength(0)
    expect(reg.size).toBe(0)
    store.dispose()
  })

  test('ignores deny-mode entries', () => {
    const reg = new SentinelRegistry()
    const store = new MaskedFileStore()
    const binds = buildMaskedFileBinds(
      [{ path: TOKEN_FILE, mode: 'deny' }],
      [],
      reg,
      store,
    )
    expect(binds).toHaveLength(0)
    store.dispose()
  })
})

/**
 * SandboxManager-level file masking on Linux: bwrap binds the fake over
 * the real path; the sandboxed process reads the sentinel; the real bytes
 * never appear in the wrapped command string.
 */
describe.if(isLinux)('file masking on Linux (bwrap)', () => {
  const TEST_DIR = join(tmpdir(), 'srt-credmask-linux-' + Date.now())
  const SECRET_FILE = join(TEST_DIR, 'token')
  const SECRET_CONTENT = 'ghp_linux_real_secret_0123456789'
  const CONTROL_FILE = join(TEST_DIR, 'control.txt')

  function baseConfig(
    overrides: Partial<SandboxRuntimeConfig> = {},
  ): SandboxRuntimeConfig {
    return {
      network: { allowedDomains: ['localhost'], deniedDomains: [] },
      filesystem: {
        denyRead: [],
        allowWrite: [TEST_DIR, '/tmp'],
        denyWrite: [],
      },
      credentials: {
        files: [{ path: SECRET_FILE, mode: 'mask' }],
        allowPlaintextInject: true,
      },
      ...overrides,
    }
  }

  function runInSandbox(wrappedCommand: string) {
    return spawnSync(wrappedCommand, {
      shell: true,
      encoding: 'utf8',
      timeout: 10000,
    })
  }

  beforeAll(async () => {
    mkdirSync(TEST_DIR, { recursive: true })
    writeFileSync(SECRET_FILE, SECRET_CONTENT)
    writeFileSync(CONTROL_FILE, 'control-ok')
    await SandboxManager.reset()
    await SandboxManager.initialize(baseConfig())
  })

  afterAll(async () => {
    await SandboxManager.reset()
    rmSync(TEST_DIR, { recursive: true, force: true })
  })

  describe('bwrap argv generation', () => {
    test('emits --ro-bind <fake> <real> with the sentinel as fake content', async () => {
      const wrapped = await SandboxManager.wrapWithSandbox('true')
      const m = wrapped.match(
        new RegExp(`--ro-bind (\\S+) ${SECRET_FILE.replace(/\//g, '\\/')}\\b`),
      )
      expect(m).not.toBeNull()
      const fakePath = m![1]!
      expect(fakePath).not.toBe('/dev/null')
      const fakeContent = readFileSync(fakePath, 'utf8')
      expect(fakeContent.startsWith(SENTINEL_PREFIX)).toBe(true)
      // The registry maps that sentinel back to the real bytes.
      expect(SandboxManager.getSentinelRegistry().lookupReal(fakeContent)).toBe(
        SECRET_CONTENT,
      )
    })

    test('the real file content never appears in the wrapped command', async () => {
      const wrapped = await SandboxManager.wrapWithSandbox('true')
      expect(wrapped).not.toContain(SECRET_CONTENT)
    })

    test('a masked file that does not exist on the host emits no bind', async () => {
      await SandboxManager.reset()
      await SandboxManager.initialize(
        baseConfig({
          credentials: {
            files: [{ path: join(TEST_DIR, 'no-such-token'), mode: 'mask' }],
            allowPlaintextInject: true,
          },
        }),
      )
      const wrapped = await SandboxManager.wrapWithSandbox('true')
      expect(wrapped).not.toContain('no-such-token')

      // Restore for the remaining tests.
      await SandboxManager.reset()
      await SandboxManager.initialize(baseConfig())
    })

    test('repeat wraps reuse the same fake file (no per-call leak)', async () => {
      await SandboxManager.wrapWithSandbox('true')
      await SandboxManager.wrapWithSandbox('true')
      const dir = SandboxManager.getMaskedFileStore().dirPath!
      expect(readdirSync(dir)).toHaveLength(1)
    })
  })

  describe('integration', () => {
    test('cat <maskedFile> inside the sandbox returns the sentinel', async () => {
      const wrapped = await SandboxManager.wrapWithSandbox(`cat ${SECRET_FILE}`)
      const result = runInSandbox(wrapped)
      expect(result.status).toBe(0)
      expect(result.stdout.startsWith(SENTINEL_PREFIX)).toBe(true)
      expect(result.stdout).not.toContain(SECRET_CONTENT)
    })

    test('the masked file is read-only inside the sandbox', async () => {
      // Even though TEST_DIR is in allowWrite, the --ro-bind on the
      // masked path layers on top — overwriting it would expose a way
      // to swap the sentinel for something the proxy might still inject.
      const wrapped = await SandboxManager.wrapWithSandbox(
        `sh -c 'echo pwned > ${SECRET_FILE}'`,
      )
      const result = runInSandbox(wrapped)
      expect(result.status).not.toBe(0)
      // Real file on the host is untouched.
      expect(readFileSync(SECRET_FILE, 'utf8')).toBe(SECRET_CONTENT)
    })

    test('a non-masked sibling file is still readable unchanged', async () => {
      const wrapped = await SandboxManager.wrapWithSandbox(
        `cat ${CONTROL_FILE}`,
      )
      const result = runInSandbox(wrapped)
      expect(result.status).toBe(0)
      expect(result.stdout).toBe('control-ok')
    })
  })

  test('reset() removes the fake-file temp dir', async () => {
    await SandboxManager.wrapWithSandbox('true')
    const dir = SandboxManager.getMaskedFileStore().dirPath
    expect(dir).toBeDefined()
    expect(existsSync(dir!)).toBe(true)
    await SandboxManager.reset()
    expect(existsSync(dir!)).toBe(false)
    expect(SandboxManager.getMaskedFileStore().dirPath).toBeUndefined()
    // Re-initialize for any following tests.
    await SandboxManager.initialize(baseConfig())
  })
})

/**
 * End-to-end: a token *file* is masked; inside the sandbox a tool reads
 * the file and sends its content as a header. The manager-started proxy
 * substitutes sentinel→real for the file's injectHost only. Reuses the
 * pattern from credential-mask.test.ts (allowPlaintextInject, plain HTTP
 * upstream, SandboxManager's own proxy port).
 */
describe.if(isLinux)('end-to-end file masking via SandboxManager', () => {
  const TEST_DIR = join(tmpdir(), 'srt-credmask-e2e-' + Date.now())
  const SECRET_FILE = join(TEST_DIR, 'token')
  const SECRET_CONTENT = 'ghp_e2e_real_secret_0123456789'
  const HOST_A = 'localhost'
  const HOST_B = 'localtest.me'

  let upstream: Server
  let upstreamPort: number
  let lastHeaders: IncomingHttpHeaders | undefined

  beforeAll(async () => {
    mkdirSync(TEST_DIR, { recursive: true })
    writeFileSync(SECRET_FILE, SECRET_CONTENT)

    upstream = createHttpServer((req, res) => {
      lastHeaders = req.headers
      res.writeHead(200)
      res.end('ok')
    })
    await new Promise<void>(r => upstream.listen(0, '127.0.0.1', () => r()))
    upstreamPort = (upstream.address() as AddressInfo).port

    await SandboxManager.reset()
    await SandboxManager.initialize({
      network: { allowedDomains: [HOST_A, HOST_B], deniedDomains: [] },
      filesystem: { denyRead: [], allowWrite: ['/tmp'], denyWrite: [] },
      credentials: {
        files: [{ path: SECRET_FILE, mode: 'mask', injectHosts: [HOST_A] }],
        allowPlaintextInject: true,
      },
    })
  })

  afterAll(async () => {
    await SandboxManager.reset()
    await new Promise<void>(r => upstream.close(() => r()))
    rmSync(TEST_DIR, { recursive: true, force: true })
  })

  // Async spawn — spawnSync would block the event loop and the
  // in-process proxy/upstream couldn't accept the connection.
  async function curlViaManagerProxy(
    url: string,
    bearer: string,
    resolve?: string,
  ): Promise<number> {
    const proxyPort = SandboxManager.getProxyPort()!
    const authToken = SandboxManager.getProxyAuthToken()!
    const args = [
      '-sS',
      '--max-time',
      '10',
      '--proxy',
      `http://srt:${authToken}@127.0.0.1:${proxyPort}`,
      '-H',
      `Authorization: Bearer ${bearer}`,
    ]
    if (resolve) args.push('--resolve', resolve)
    args.push(url)
    const child = spawn('curl', args)
    child.stdout.on('data', () => {})
    child.stderr.on('data', () => {})
    return new Promise(r => child.on('close', code => r(code ?? 1)))
  }

  test('cat inside the sandbox + manager proxy → injectHost gets real bytes', async () => {
    // bwrap leg: cat inside the sandbox returns the sentinel.
    const wrapped = await SandboxManager.wrapWithSandbox(`cat ${SECRET_FILE}`)
    expect(wrapped).not.toContain(SECRET_CONTENT)
    const inSandbox = spawnSync(wrapped, {
      shell: true,
      encoding: 'utf8',
      timeout: 10000,
    })
    expect(inSandbox.status).toBe(0)
    const sentinel = inSandbox.stdout
    expect(sentinel.startsWith(SENTINEL_PREFIX)).toBe(true)
    expect(sentinel).not.toContain(SECRET_CONTENT)

    // Proxy leg: the same sentinel sent through the manager-started
    // proxy reaches HOST_A (injectHost) as the real file content.
    lastHeaders = undefined
    const exit = await curlViaManagerProxy(
      `http://${HOST_A}:${upstreamPort}/`,
      sentinel,
    )
    expect(exit).toBe(0)
    expect(lastHeaders?.authorization).toBe(`Bearer ${SECRET_CONTENT}`)
  }, 20000)

  test('a non-injectHost destination receives the sentinel unchanged', async () => {
    const wrapped = await SandboxManager.wrapWithSandbox(`cat ${SECRET_FILE}`)
    const sentinel = spawnSync(wrapped, {
      shell: true,
      encoding: 'utf8',
      timeout: 10000,
    }).stdout

    // HOST_B is allowlisted but NOT in this file's injectHosts. The
    // proxy dials localtest.me (publicly resolves to 127.0.0.1) and
    // forwards the sentinel as-is — fails closed.
    lastHeaders = undefined
    const exit = await curlViaManagerProxy(
      `http://${HOST_B}:${upstreamPort}/`,
      sentinel,
      `${HOST_B}:${upstreamPort}:127.0.0.1`,
    )
    expect(exit).toBe(0)
    expect(lastHeaders?.authorization).toBe(`Bearer ${sentinel}`)
    expect(lastHeaders?.authorization).not.toContain(SECRET_CONTENT)
  }, 20000)
})
