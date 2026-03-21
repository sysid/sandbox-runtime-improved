import { describe, it, expect, beforeEach, afterEach } from 'bun:test'
import { generateProxyEnvVars } from '../../src/sandbox/sandbox-utils.js'

describe('generateProxyEnvVars', () => {
  let originalNodeOptions: string | undefined

  beforeEach(() => {
    originalNodeOptions = process.env.NODE_OPTIONS
  })

  afterEach(() => {
    if (originalNodeOptions === undefined) {
      delete process.env.NODE_OPTIONS
    } else {
      process.env.NODE_OPTIONS = originalNodeOptions
    }
  })

  it('sets CLOUDSDK_PROXY_TYPE to http (gcloud rejects "https")', () => {
    // gcloud's proxy/type only accepts http, http_no_tunnel, socks4, socks5.
    // Our local proxy is an HTTP CONNECT proxy regardless of the traffic it
    // tunnels, so the value must be "http" — see issue #151.
    const env = generateProxyEnvVars(3128, 1080)

    expect(env).toContain('CLOUDSDK_PROXY_TYPE=http')
    expect(env).toContain('CLOUDSDK_PROXY_ADDRESS=localhost')
    expect(env).toContain('CLOUDSDK_PROXY_PORT=3128')
    expect(env).not.toContain('CLOUDSDK_PROXY_TYPE=https')
  })

  it('omits CLOUDSDK_PROXY_* when no HTTP proxy port is configured', () => {
    const env = generateProxyEnvVars(undefined, 1080)

    expect(env.some(v => v.startsWith('CLOUDSDK_PROXY_'))).toBe(false)
  })

  it('should return minimal env vars when no proxy ports provided', () => {
    const vars = generateProxyEnvVars()

    expect(vars).toContainEqual(expect.stringMatching(/^SANDBOX_RUNTIME=1$/))
    expect(vars).toContainEqual(expect.stringMatching(/^TMPDIR=/))
    // No proxy-related vars
    expect(vars.find(v => v.startsWith('HTTP_PROXY='))).toBeUndefined()
    expect(vars.find(v => v.startsWith('NODE_OPTIONS='))).toBeUndefined()
  })

  it('should include NODE_OPTIONS with --use-env-proxy when httpProxyPort is set', () => {
    delete process.env.NODE_OPTIONS

    const vars = generateProxyEnvVars(8080)
    const nodeOpts = vars.find(v => v.startsWith('NODE_OPTIONS='))

    // Node 22+ should have this
    const nodeMajor = parseInt(process.versions.node.split('.')[0], 10)
    if (nodeMajor >= 22) {
      expect(nodeOpts).toBe('NODE_OPTIONS=--use-env-proxy')
    } else {
      expect(nodeOpts).toBeUndefined()
    }
  })

  it('should preserve existing NODE_OPTIONS when adding --use-env-proxy', () => {
    process.env.NODE_OPTIONS = '--max-old-space-size=4096'

    const vars = generateProxyEnvVars(8080)
    const nodeOpts = vars.find(v => v.startsWith('NODE_OPTIONS='))

    const nodeMajor = parseInt(process.versions.node.split('.')[0], 10)
    if (nodeMajor >= 22) {
      expect(nodeOpts).toBe(
        'NODE_OPTIONS=--max-old-space-size=4096 --use-env-proxy',
      )
    } else {
      expect(nodeOpts).toBeUndefined()
    }
  })

  it('should include HTTP_PROXY and HTTPS_PROXY when httpProxyPort is set', () => {
    const vars = generateProxyEnvVars(8080)

    expect(vars).toContainEqual('HTTP_PROXY=http://localhost:8080')
    expect(vars).toContainEqual('HTTPS_PROXY=http://localhost:8080')
    expect(vars).toContainEqual('http_proxy=http://localhost:8080')
    expect(vars).toContainEqual('https_proxy=http://localhost:8080')
  })

  it('should include NO_PROXY when proxy ports are set', () => {
    const vars = generateProxyEnvVars(8080)

    const noProxy = vars.find(v => v.startsWith('NO_PROXY='))
    expect(noProxy).toBeDefined()
    expect(noProxy).toContain('localhost')
    expect(noProxy).toContain('127.0.0.1')
  })

  it('should include SOCKS proxy vars when socksProxyPort is set', () => {
    const vars = generateProxyEnvVars(undefined, 1080)

    expect(vars).toContainEqual('ALL_PROXY=socks5h://localhost:1080')
    expect(vars).toContainEqual('all_proxy=socks5h://localhost:1080')
  })
})
