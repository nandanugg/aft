declare module "bun:test" {
  export const afterEach: (...args: any[]) => any;
  export const describe: (...args: any[]) => any;
  export const expect: (...args: any[]) => any;
  export const mock: (...args: any[]) => any;
  export const test: (...args: any[]) => any;
}

interface ImportMeta {
  readonly dir: string;
}
