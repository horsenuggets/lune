import { ReadB } from "./CycleB.js";

export function ReadCycle(): string {
    return `A${ReadB()}`;
}
