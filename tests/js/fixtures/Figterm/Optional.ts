export const Marker = 1;

export async function Load() {
    return import("./DoesNotExist.js");
}
