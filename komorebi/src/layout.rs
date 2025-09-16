use crate::rect::Rect;

pub fn recursive_fibonacci(
    idx: usize,
    count: usize,
    area: &Rect,
    resize_adjustments: Vec<Option<Rect>>,
) -> Vec<Rect> {
    let mut a = *area;

    let resized = if let Some(Some(r)) = resize_adjustments.get(idx) {
        a.left += r.left;
        a.top += r.top;
        a.right += r.right;
        a.bottom += r.bottom;
        a
    } else {
        *area
    };

    // let half_width = area.right / 2.0;
    // let half_height = area.bottom / 2.0;
    let half_resized_width = resized.right / 2.0;
    let half_resized_height = resized.bottom / 2.0;

    let (main_x, alt_x, alt_y, main_y);

    main_x = resized.left;
    alt_x = resized.left + half_resized_width;
    main_y = resized.top;
    alt_y = resized.top + half_resized_height;

    #[allow(clippy::if_not_else)]
    if count == 0 {
        vec![]
    } else if count == 1 {
        vec![Rect {
            left: resized.left,
            top: resized.top,
            right: resized.right,
            bottom: resized.bottom,
        }]
    } else if idx % 2 != 0 {
        let mut res = vec![Rect {
            left: resized.left,
            top: main_y,
            right: resized.right,
            bottom: half_resized_height,
        }];
        res.append(&mut recursive_fibonacci(
            idx + 1,
            count - 1,
            &Rect {
                left: area.left,
                top: alt_y,
                right: area.right,
                bottom: area.bottom - half_resized_height,
            },
            resize_adjustments,
        ));
        res
    } else {
        let mut res = vec![Rect {
            left: main_x,
            top: resized.top,
            right: half_resized_width,
            bottom: resized.bottom,
        }];
        res.append(&mut recursive_fibonacci(
            idx + 1,
            count - 1,
            &Rect {
                left: alt_x,
                top: area.top,
                right: area.right - half_resized_width,
                bottom: area.bottom,
            },
            resize_adjustments,
        ));
        res
    }
}
