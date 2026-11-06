// A module-level `globalThis` binding must not break computed imports.
export const globalThis = { Marker: 1 };

export async function Load(Name: string) {
    return import(`./${Name}.js`);
}
