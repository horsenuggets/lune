export async function Load(Name: string) {
    return import(`./${Name}.js`);
}
