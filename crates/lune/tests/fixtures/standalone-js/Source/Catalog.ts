import type { Story } from "./Types.js";

declare function HostFormatStory(Name: string, Projects: string[]): Promise<string>;

export function CreateCatalog(Component: string, Stories: Story[]): Story[] {
    return Stories.map((Story) => ({
        ...Story,
        Name: `${Component}.${Story.Name}`,
    }));
}

export async function RenderStory(Story: Story): Promise<string> {
    const Summary = await HostFormatStory(Story.Name, Story.Projects);
    return `${Summary} [${Story.Terminal.Columns}x${Story.Terminal.Rows}]`;
}
