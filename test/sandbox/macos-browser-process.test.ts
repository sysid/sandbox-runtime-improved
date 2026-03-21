import { describe, it, expect, beforeAll, afterAll } from 'bun:test'
import { existsSync, mkdirSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { getPlatform } from '../../src/utils/platform.js'
import { wrapCommandWithSandboxMacOS } from '../../src/sandbox/macos-sandbox-utils.js'
import type { FsWriteRestrictionConfig } from '../../src/sandbox/sandbox-schemas.js'

function skipIfNotMacOS(): boolean {
  return getPlatform() !== 'macos'
}

describe('macOS Seatbelt Browser Process Support', () => {
  const TEST_BASE_DIR = join(tmpdir(), 'seatbelt-browser-test-' + Date.now())

  beforeAll(() => {
    if (skipIfNotMacOS()) {
      return
    }
    mkdirSync(TEST_BASE_DIR, { recursive: true })
  })

  afterAll(() => {
    if (skipIfNotMacOS()) {
      return
    }
    if (existsSync(TEST_BASE_DIR)) {
      rmSync(TEST_BASE_DIR, { recursive: true, force: true })
    }
  })

  it('should include browser Seatbelt rules when allowBrowserProcess is true', () => {
    if (skipIfNotMacOS()) {
      return
    }

    const writeConfig: FsWriteRestrictionConfig = {
      allowOnly: [TEST_BASE_DIR],
    }

    // Generate the wrapped command and extract the profile from it
    const wrappedCommand = wrapCommandWithSandboxMacOS({
      command: 'echo browser-test',
      needsNetworkRestriction: false,
      readConfig: undefined,
      writeConfig,
      allowBrowserProcess: true,
    })

    // The profile is embedded in the sandbox-exec command — verify it contains
    // the expected Seatbelt rules by checking the generated command string
    expect(wrappedCommand).toContain('(allow mach*)')
    expect(wrappedCommand).toContain('(allow process-info*)')
    expect(wrappedCommand).toContain('(allow iokit-open)')
    expect(wrappedCommand).toContain('(allow ipc-posix-shm*)')
  })

  it('should NOT include browser Seatbelt rules when allowBrowserProcess is false', () => {
    if (skipIfNotMacOS()) {
      return
    }

    const writeConfig: FsWriteRestrictionConfig = {
      allowOnly: [TEST_BASE_DIR],
    }

    const wrappedCommand = wrapCommandWithSandboxMacOS({
      command: 'echo browser-test',
      needsNetworkRestriction: false,
      readConfig: undefined,
      writeConfig,
      allowBrowserProcess: false,
    })

    expect(wrappedCommand).not.toContain('(allow mach*)')
    expect(wrappedCommand).not.toContain('(allow process-info*)')
    expect(wrappedCommand).not.toContain('(allow iokit-open)')
    expect(wrappedCommand).not.toContain('(allow ipc-posix-shm*)')
  })

  it('should NOT include browser rules by default (omitted flag)', () => {
    if (skipIfNotMacOS()) {
      return
    }

    const writeConfig: FsWriteRestrictionConfig = {
      allowOnly: [TEST_BASE_DIR],
    }

    const wrappedCommand = wrapCommandWithSandboxMacOS({
      command: 'echo browser-test',
      needsNetworkRestriction: false,
      readConfig: undefined,
      writeConfig,
    })

    expect(wrappedCommand).not.toContain('(allow mach*)')
    expect(wrappedCommand).not.toContain('(allow ipc-posix-shm*)')
  })

  it('should not affect other sandbox rules when allowBrowserProcess is true', () => {
    if (skipIfNotMacOS()) {
      return
    }

    const writeConfig: FsWriteRestrictionConfig = {
      allowOnly: [TEST_BASE_DIR],
    }

    const wrappedCommand = wrapCommandWithSandboxMacOS({
      command: 'echo test',
      needsNetworkRestriction: true,
      httpProxyPort: 8080,
      readConfig: undefined,
      writeConfig,
      allowBrowserProcess: true,
    })

    // Browser rules present
    expect(wrappedCommand).toContain('(allow mach*)')
    // Network and filesystem rules still present
    expect(wrappedCommand).toContain('deny default')
    expect(wrappedCommand).toContain(TEST_BASE_DIR)
  })
})
