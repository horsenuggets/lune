// `import()` coerces its specifier with ToString, including boxed strings.
export async function LoadBoxed() {
    return import(new String("./Extra.js") as unknown as string);
}
