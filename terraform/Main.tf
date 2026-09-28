terraform {
  required_version = ">= 1.0"

  required_providers {
    github = {
      source  = "integrations/github"
      version = "~> 6.0"
    }
  }
}

provider "github" {
  owner = "horsenuggets"
}

module "repo" {
  source     = "../submodules/luau-cicd/Terraform/Modules/LuauRepo"
  repository = "lune"

  main_checks = [
    "Analyze and lint Luau files",
    "Check formatting",
    "CI - Linux aarch64",
    "CI - Linux x86_64",
    "CI - macOS aarch64",
    "CI - macOS x86_64",
    "CI - Windows aarch64",
    "CI - Windows x86_64",
  ]

  release_checks = [
    "Build test - Linux aarch64",
    "Build test - Linux x86_64",
    "Build test - macOS aarch64",
    "Build test - macOS x86_64",
    "Build test - Windows aarch64",
    "Build test - Windows x86_64",
    "Validate PR title",
    "Validate version",
    "Verify diff matches main",
  ]
}

# Automatically request a Copilot code review on every non-draft PR targeting
# main or release, matching the established horsenuggets convention. The
# LuauRepo module only configures branch protection and status checks, so the
# Copilot review gate lives in its own ruleset here.
resource "github_repository_ruleset" "copilot_code_review" {
  name        = "Copilot Code Review"
  repository  = "lune"
  target      = "branch"
  enforcement = "active"

  conditions {
    ref_name {
      include = ["refs/heads/main", "refs/heads/release"]
      exclude = []
    }
  }

  rules {
    copilot_code_review {
      review_on_push             = true
      review_draft_pull_requests = false
    }
  }
}
