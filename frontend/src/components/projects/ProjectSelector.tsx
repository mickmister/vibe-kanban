import { useNavigate } from 'react-router-dom';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select';
import { paths } from '@/lib/paths';
import type { Project } from 'shared/types';

interface ProjectSelectorProps {
  projects: Project[];
  currentProjectId: string;
}

export function ProjectSelector({
  projects,
  currentProjectId,
}: ProjectSelectorProps) {
  const navigate = useNavigate();

  const handleProjectChange = (projectId: string) => {
    navigate(paths.projectTasks(projectId));
  };

  return (
    <Select value={currentProjectId} onValueChange={handleProjectChange}>
      <SelectTrigger className="w-[200px] h-8">
        <SelectValue placeholder="Select a project" />
      </SelectTrigger>
      <SelectContent>
        {projects.map((project) => (
          <SelectItem key={project.id} value={project.id}>
            {project.name}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}
