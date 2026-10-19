use super::*;
use crate::memory::ChannelAccess;
use crate::types::IMAGE_LENGTH;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Image address of group 0's name in a firmware 1.02 backup.
const FIRST_NAME: usize = 0x1_4800;

fn name(text: &str) -> [u8; CHANNEL_NAME_SIZE] {
    let mut bytes = [0; CHANNEL_NAME_SIZE];
    for (destination, source) in bytes.iter_mut().zip(text.bytes()) {
        *destination = source;
    }
    bytes
}

fn put(image: &mut [u8], offset: usize, bytes: &[u8]) -> TestResult {
    image
        .get_mut(offset..offset + bytes.len())
        .ok_or("offset inside the image")?
        .copy_from_slice(bytes);
    Ok(())
}

/// A full image of `0x42` holding the names firmware 1.02 stores by default:
/// `GRP-0` to `GRP-29`, then `Weather`.
fn default_names() -> Result<MemoryImage, Box<dyn std::error::Error>> {
    let mut image = vec![0x42; IMAGE_LENGTH];
    for index in 0..30 {
        put(
            &mut image,
            FIRST_NAME + 16 * index,
            &name(&format!("GRP-{index}")),
        )?;
    }
    put(&mut image, 0x1_49E0, &name("Weather"))?;
    Ok(MemoryImage::from_bytes(image)?)
}

fn group(index: u8) -> Result<MemoryGroup, ValidationError> {
    MemoryGroup::new(index)
}

#[test]
fn group_names_follow_the_call_names_in_the_name_table() -> TestResult {
    assert_eq!(group(0)?.name_address(), 0x1_4800);
    assert_eq!(group(29)?.name_address(), 0x1_49D0);
    assert_eq!(MemoryGroup::all().count(), 30);
    assert_eq!(group(7)?.to_string(), "group 7");
    assert_eq!(
        MemoryGroup::new(30),
        Err(ValidationError::SettingOutOfRange {
            setting: "memory group",
            value: 30,
            max: 29
        })
    );
    assert_eq!(ChannelAccess::REQUIRED_LENGTH, 0x1_49F0);
    Ok(())
}

#[test]
fn channel_access_reads_every_group_name_and_the_weather_group_name() -> TestResult {
    let image = default_names()?;
    let access = ChannelAccess::from_memory_image(&image);
    assert_eq!(access.group_name(group(5)?).text(), "GRP-5");
    let names: Vec<String> = access
        .group_names()
        .map(|(group, name)| format!("{}={}", group.index(), name.text()))
        .collect();
    assert_eq!(names.len(), 30);
    assert_eq!(names.last().map(String::as_str), Some("29=GRP-29"));
    assert_eq!(access.weather_group_name().text(), "Weather");
    Ok(())
}

#[test]
fn a_plan_rewrites_only_its_groups_names() -> TestResult {
    let image = default_names()?;
    let plan = GroupNamePlan::new([
        (group(3)?, ChannelNameText::new("Repeaters")?),
        (group(20)?, ChannelNameText::new("Simplex 2m")?),
    ])?;
    assert_eq!(
        plan.pages()
            .iter()
            .map(|page| page.address().as_u32())
            .collect::<Vec<_>>(),
        [0x1_4800, 0x1_4900]
    );
    let replacements = plan.page_replacements(&image)?;
    let mut changed = Vec::new();
    for replacement in &replacements {
        let start = replacement.page().address().as_usize();
        assert_eq!(replacement.page().len(), 256);
        for (offset, (before, after)) in replacement
            .expected()
            .iter()
            .zip(replacement.replacement())
            .enumerate()
        {
            if before != after {
                changed.push(start + offset);
            }
        }
    }
    let repeaters = 0x1_4830..0x1_4839;
    let simplex = 0x1_4940..0x1_494A;
    assert!(
        changed
            .iter()
            .all(|address| repeaters.contains(address) || simplex.contains(address)),
        "{changed:X?}"
    );
    assert!(changed.contains(&0x1_4838) && changed.contains(&0x1_4949));

    let unchanged = GroupNamePlan::new([(group(29)?, ChannelNameText::new("GRP-29")?)])?;
    assert_eq!(unchanged.pages().len(), 1);
    assert!(unchanged.page_replacements(&image)?.is_empty());
    Ok(())
}

#[test]
fn a_plan_names_each_group_once() -> TestResult {
    let result = GroupNamePlan::new([
        (group(4)?, ChannelNameText::new("A")?),
        (group(4)?, ChannelNameText::new("B")?),
    ]);
    assert_eq!(
        result,
        Err(ValidationError::DuplicateGroupName { group: group(4)? })
    );
    Ok(())
}
