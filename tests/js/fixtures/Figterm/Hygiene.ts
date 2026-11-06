// A local binding with the runtime helper's name must not capture computed imports.
const __luneDynamicImport = () => {
    throw new Error("shadowed helper");
};

export const Helper = __luneDynamicImport;

export async function Load(Name: string) {
    return import(`./${Name}.js`);
}
