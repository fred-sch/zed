use std::path::PathBuf;

use anyhow::Context as _;
use gpui::{
    canvas, div, fill, img, opaque_grey, point, size, AnyElement, AppContext, Bounds, EventEmitter,
    FocusHandle, FocusableView, InteractiveElement, IntoElement, Model, ObjectFit, ParentElement,
    Render, Styled, Task, View, ViewContext, VisualContext, WeakView, WindowContext,
};
use persistence::IMAGE_VIEWER;
use theme::Theme;
use ui::prelude::*;

use file_icons::FileIcons;
use project::{image_store::ImageItemEvent, ImageItem, Project, ProjectPath};
use settings::Settings;
use util::paths::PathExt;
use workspace::{
    item::{BreadcrumbText, Item, ProjectItem, SerializableItem, TabContentParams},
    ItemId, ItemSettings, ToolbarItemLocation, Workspace, WorkspaceId,
};

const IMAGE_VIEWER_KIND: &str = "ImageView";

pub struct ImageView {
    image_item: Model<ImageItem>,
    project: Model<Project>,
    focus_handle: FocusHandle,
}

impl ImageView {
    pub fn new(
        image_item: Model<ImageItem>,
        project: Model<Project>,
        cx: &mut ViewContext<Self>,
    ) -> Self {
        cx.subscribe(&image_item, Self::on_image_event).detach();
        Self {
            image_item,
            project,
            focus_handle: cx.focus_handle(),
        }
    }

    fn on_image_event(
        &mut self,
        _: Model<ImageItem>,
        event: &ImageItemEvent,
        cx: &mut ViewContext<Self>,
    ) {
        match event {
            ImageItemEvent::FileHandleChanged | ImageItemEvent::Reloaded => {
                cx.emit(ImageViewEvent::TitleChanged);
                cx.notify();
            }
            ImageItemEvent::ReloadNeeded => {}
        }
    }
}

pub enum ImageViewEvent {
    TitleChanged,
}

impl EventEmitter<ImageViewEvent> for ImageView {}

impl Item for ImageView {
    type Event = ImageViewEvent;

    fn to_item_events(event: &Self::Event, mut f: impl FnMut(workspace::item::ItemEvent)) {
        match event {
            ImageViewEvent::TitleChanged => {
                f(workspace::item::ItemEvent::UpdateTab);
                f(workspace::item::ItemEvent::UpdateBreadcrumbs);
            }
        }
    }

    fn for_each_project_item(
        &self,
        cx: &AppContext,
        f: &mut dyn FnMut(gpui::EntityId, &dyn project::Item),
    ) {
        f(self.image_item.entity_id(), self.image_item.read(cx))
    }

    fn is_singleton(&self, _cx: &AppContext) -> bool {
        true
    }

    fn tab_tooltip_text(&self, cx: &AppContext) -> Option<SharedString> {
        let abs_path = self.image_item.read(cx).file.as_local()?.abs_path(cx);
        let file_path = abs_path.compact().to_string_lossy().to_string();
        Some(file_path.into())
    }

    fn tab_content(&self, params: TabContentParams, cx: &WindowContext) -> AnyElement {
        let path = self.image_item.read(cx).file.path();
        let title = path
            .file_name()
            .unwrap_or_else(|| path.as_os_str())
            .to_string_lossy()
            .to_string();
        Label::new(title)
            .single_line()
            .color(params.text_color())
            .italic(params.preview)
            .into_any_element()
    }

    fn tab_icon(&self, cx: &WindowContext) -> Option<Icon> {
        let path = self.image_item.read(cx).path();
        ItemSettings::get_global(cx)
            .file_icons
            .then(|| FileIcons::get_icon(path, cx))
            .flatten()
            .map(Icon::from_path)
    }

    fn breadcrumb_location(&self, _: &AppContext) -> ToolbarItemLocation {
        ToolbarItemLocation::PrimaryLeft
    }

    fn breadcrumbs(&self, _theme: &Theme, cx: &AppContext) -> Option<Vec<BreadcrumbText>> {
        let text = breadcrumbs_text_for_image(self.project.read(cx), self.image_item.read(cx), cx);
        Some(vec![BreadcrumbText {
            text,
            highlights: None,
            font: None,
        }])
    }

    fn clone_on_split(
        &self,
        _workspace_id: Option<WorkspaceId>,
        cx: &mut ViewContext<Self>,
    ) -> Option<View<Self>>
    where
        Self: Sized,
    {
        Some(cx.new_view(|cx| Self {
            image_item: self.image_item.clone(),
            project: self.project.clone(),
            focus_handle: cx.focus_handle(),
        }))
    }
}

fn breadcrumbs_text_for_image(project: &Project, image: &ImageItem, cx: &AppContext) -> String {
    let path = image.path();
    if project.visible_worktrees(cx).count() <= 1 {
        return path.to_string_lossy().to_string();
    }

    project
        .worktree_for_id(image.project_path(cx).worktree_id, cx)
        .map(|worktree| {
            PathBuf::from(worktree.read(cx).root_name())
                .join(path)
                .to_string_lossy()
                .to_string()
        })
        .unwrap_or_else(|| path.to_string_lossy().to_string())
}

impl SerializableItem for ImageView {
    fn serialized_item_kind() -> &'static str {
        IMAGE_VIEWER_KIND
    }

    fn deserialize(
        project: Model<Project>,
        _workspace: WeakView<Workspace>,
        workspace_id: WorkspaceId,
        item_id: ItemId,
        cx: &mut WindowContext,
    ) -> Task<gpui::Result<View<Self>>> {
        cx.spawn(|mut cx| async move {
            let image_path = IMAGE_VIEWER
                .get_image_path(item_id, workspace_id)?
                .ok_or_else(|| anyhow::anyhow!("No image path found"))?;

            let (worktree, relative_path) = project
                .update(&mut cx, |project, cx| {
                    project.find_or_create_worktree(image_path.clone(), false, cx)
                })?
                .await
                .context("Path not found")?;
            let worktree_id = worktree.update(&mut cx, |worktree, _cx| worktree.id())?;

            let project_path = ProjectPath {
                worktree_id,
                path: relative_path.into(),
            };

            let image_item = project
                .update(&mut cx, |project, cx| project.open_image(project_path, cx))?
                .await?;

            cx.update(|cx| Ok(cx.new_view(|cx| ImageView::new(image_item, project, cx))))?
        })
    }

    fn cleanup(
        workspace_id: WorkspaceId,
        alive_items: Vec<ItemId>,
        cx: &mut WindowContext,
    ) -> Task<gpui::Result<()>> {
        cx.spawn(|_| IMAGE_VIEWER.delete_unloaded_items(workspace_id, alive_items))
    }

    fn serialize(
        &mut self,
        workspace: &mut Workspace,
        item_id: ItemId,
        _closing: bool,
        cx: &mut ViewContext<Self>,
    ) -> Option<Task<gpui::Result<()>>> {
        let workspace_id = workspace.database_id()?;
        let image_path = self.image_item.read(cx).file.as_local()?.abs_path(cx);

        Some(cx.background_executor().spawn({
            async move {
                IMAGE_VIEWER
                    .save_image_path(item_id, workspace_id, image_path)
                    .await
            }
        }))
    }

    fn should_serialize(&self, _event: &Self::Event) -> bool {
        false
    }
}

impl EventEmitter<()> for ImageView {}
impl FocusableView for ImageView {
    fn focus_handle(&self, _cx: &AppContext) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ImageView {
    fn render(&mut self, cx: &mut ViewContext<Self>) -> impl IntoElement {
        let image = self.image_item.read(cx).image.clone();
        let checkered_background = |bounds: Bounds<Pixels>, _, cx: &mut WindowContext| {
            let square_size = 32.0;

            let start_y = bounds.origin.y.0;
            let height = bounds.size.height.0;
            let start_x = bounds.origin.x.0;
            let width = bounds.size.width.0;

            let mut y = start_y;
            let mut x = start_x;
            let mut color_swapper = true;
            // draw checkerboard pattern
            while y <= start_y + height {
                // Keeping track of the grid in order to be resilient to resizing
                let start_swap = color_swapper;
                while x <= start_x + width {
                    let rect =
                        Bounds::new(point(px(x), px(y)), size(px(square_size), px(square_size)));

                    let color = if color_swapper {
                        opaque_grey(0.6, 0.4)
                    } else {
                        opaque_grey(0.7, 0.4)
                    };

                    cx.paint_quad(fill(rect, color));
                    color_swapper = !color_swapper;
                    x += square_size;
                }
                x = start_x;
                color_swapper = !start_swap;
                y += square_size;
            }
        };

        let checkered_background = canvas(|_, _| (), checkered_background)
            .border_2()
            .border_color(cx.theme().styles.colors.border)
            .size_full()
            .absolute()
            .top_0()
            .left_0();

        div()
            .track_focus(&self.focus_handle(cx))
            .size_full()
            .child(checkered_background)
            .child(
                div()
                    .flex()
                    .justify_center()
                    .items_center()
                    .w_full()
                    // TODO: In browser based Tailwind & Flex this would be h-screen and we'd use w-full
                    .h_full()
                    .child(
                        img(image)
                            .object_fit(ObjectFit::ScaleDown)
                            .max_w_full()
                            .max_h_full()
                            .id("img"),
                    ),
            )
    }
}

impl ProjectItem for ImageView {
    type Item = ImageItem;

    fn for_project_item(
        project: Model<Project>,
        item: Model<Self::Item>,
        cx: &mut ViewContext<Self>,
    ) -> Self
    where
        Self: Sized,
    {
        Self::new(item, project, cx)
    }
}

pub fn init(cx: &mut AppContext) {
    workspace::register_project_item::<ImageView>(cx);
    workspace::register_serializable_item::<ImageView>(cx)
}

mod persistence {
    use anyhow::Result;
    use std::path::PathBuf;

    use db::{define_connection, query, sqlez::statement::Statement, sqlez_macros::sql};
    use workspace::{ItemId, WorkspaceDb, WorkspaceId};

    define_connection! {
        pub static ref IMAGE_VIEWER: ImageViewerDb<WorkspaceDb> =
            &[sql!(
                CREATE TABLE image_viewers (
                    workspace_id INTEGER,
                    item_id INTEGER UNIQUE,

                    image_path BLOB,

                    PRIMARY KEY(workspace_id, item_id),
                    FOREIGN KEY(workspace_id) REFERENCES workspaces(workspace_id)
                    ON DELETE CASCADE
                ) STRICT;
            )];
    }

    impl ImageViewerDb {
        query! {
           pub async fn update_workspace_id(
                new_id: WorkspaceId,
                old_id: WorkspaceId,
                item_id: ItemId
            ) -> Result<()> {
                UPDATE image_viewers
                SET workspace_id = ?
                WHERE workspace_id = ? AND item_id = ?
            }
        }

        query! {
            pub async fn save_image_path(
                item_id: ItemId,
                workspace_id: WorkspaceId,
                image_path: PathBuf
            ) -> Result<()> {
                INSERT OR REPLACE INTO image_viewers(item_id, workspace_id, image_path)
                VALUES (?, ?, ?)
            }
        }

        query! {
            pub fn get_image_path(item_id: ItemId, workspace_id: WorkspaceId) -> Result<Option<PathBuf>> {
                SELECT image_path
                FROM image_viewers
                WHERE item_id = ? AND workspace_id = ?
            }
        }

        pub async fn delete_unloaded_items(
            &self,
            workspace: WorkspaceId,
            alive_items: Vec<ItemId>,
        ) -> Result<()> {
            let placeholders = alive_items
                .iter()
                .map(|_| "?")
                .collect::<Vec<&str>>()
                .join(", ");

            let query = format!("DELETE FROM image_viewers WHERE workspace_id = ? AND item_id NOT IN ({placeholders})");

            self.write(move |conn| {
                let mut statement = Statement::prepare(conn, query)?;
                let mut next_index = statement.bind(&workspace, 1)?;
                for id in alive_items {
                    next_index = statement.bind(&id, next_index)?;
                }
                statement.exec()
            })
            .await
        }
    }
}
