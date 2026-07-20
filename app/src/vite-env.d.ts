/// <reference types="vite/client" />

interface ImportMetaEnv {
  readonly VITE_BACKEND_URL?: string
  readonly VITE_ARCH_RPC_URL?: string
  readonly VITE_PROGRAM_ID?: string
  readonly VITE_ENABLE_DEMO?: string
}
interface ImportMeta {
  readonly env: ImportMetaEnv
}
