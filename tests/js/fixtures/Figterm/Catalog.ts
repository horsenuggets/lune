import type { Story } from "./Story.js";

export function CreateCatalog(Component: string, Stories: Story[]): Story[] {
    return Stories.map((Story) => ({
        ...Story,
        Name: `${Component}.${Story.Name}`,
    }));
}

export function CountProjects(Story: Story): number {
    return Story.Projects.length;
}
