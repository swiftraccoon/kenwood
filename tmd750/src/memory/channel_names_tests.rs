use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn channel(index: u16) -> Result<PhysicalChannel, Box<dyn std::error::Error>> {
    Ok(PhysicalChannel::new(index)?)
}

fn name(text: &str) -> Result<Option<ChannelNameText>, Box<dyn std::error::Error>> {
    Ok(Some(ChannelNameText::new(text)?))
}

/// A blank image whose name table holds `named` channels.
fn image(named: &[(u16, &str)]) -> Result<MemoryImage, Box<dyn std::error::Error>> {
    let mut raw = MemoryImage::blank().into_bytes();
    for index in 0..crate::types::PHYSICAL_CHANNEL_COUNT {
        let start = channel(index)?.name_address();
        raw.get_mut(start..start + CHANNEL_NAME_SIZE)
            .ok_or("inside the image")?
            .fill(0);
    }
    for (index, text) in named {
        let start = channel(*index)?.name_address();
        raw.get_mut(start..start + CHANNEL_NAME_SIZE)
            .ok_or("inside the image")?
            .copy_from_slice(&ChannelNameText::new(text)?.to_field());
    }
    Ok(MemoryImage::from_bytes(raw)?)
}

#[test]
fn names_on_one_page_share_one_write() -> TestResult {
    let captured = image(&[(15, "Old")])?;
    let plan = ChannelNamePlan::new([
        (channel(0)?, name("Repeater 1")?),
        (channel(15)?, name("Simplex")?),
    ])?;
    assert_eq!(
        plan.pages()
            .iter()
            .map(|page| page.address().as_u32())
            .collect::<Vec<_>>(),
        [0x1_0000]
    );
    let replacements = plan.page_replacements(&captured)?;
    let [replacement] = replacements.as_slice() else {
        return Err(format!("{} replacements", replacements.len()).into());
    };
    assert_eq!(replacement.page().address().as_u32(), 0x1_0000);
    assert_eq!(
        replacement.replacement().get(..16),
        Some(&b"Repeater 1\0\0\0\0\0\0"[..])
    );
    assert_eq!(
        replacement.replacement().get(240..),
        Some(&b"Simplex\0\0\0\0\0\0\0\0\0"[..])
    );
    assert_eq!(
        replacement.replacement().get(16..240),
        replacement.expected().get(16..240),
        "the other names on the page are kept"
    );
    Ok(())
}

#[test]
fn a_cleared_name_stores_sixteen_nul_bytes_and_an_unchanged_one_plans_no_write() -> TestResult {
    let captured = image(&[(999, "Repeater 1"), (17, "Kept")])?;
    let cleared = ChannelNamePlan::new([(channel(999)?, None)])?.page_replacements(&captured)?;
    let [replacement] = cleared.as_slice() else {
        return Err(format!("{} replacements", cleared.len()).into());
    };
    let offset = channel(999)?.name_address() - replacement.page().address().as_usize();
    assert_eq!(
        replacement
            .replacement()
            .get(offset..offset + CHANNEL_NAME_SIZE),
        Some(&[0; CHANNEL_NAME_SIZE][..])
    );
    let unchanged = ChannelNamePlan::new([(channel(17)?, name("Kept")?), (channel(18)?, None)])?;
    assert_eq!(unchanged.page_replacements(&captured)?, []);
    Ok(())
}

#[test]
fn pages_list_each_touched_page_once_in_address_order() -> TestResult {
    let plan = ChannelNamePlan::new([
        (channel(1101)?, name("APRS")?),
        (channel(17)?, None),
        (channel(16)?, name("Sixteen")?),
        (channel(0)?, None),
    ])?;
    let pages: Vec<u32> = plan
        .pages()
        .iter()
        .map(|page| page.address().as_u32())
        .collect();
    assert_eq!(pages, [0x1_0000, 0x1_0100, 0x1_4400]);
    assert!(plan.pages().iter().all(|page| page.len() == PAGE_SIZE));
    Ok(())
}

#[test]
fn a_channel_named_twice_is_refused() -> TestResult {
    let result = ChannelNamePlan::new([(channel(5)?, name("One")?), (channel(5)?, None)]);
    assert!(
        matches!(
            result,
            Err(ValidationError::DuplicateChannelName { index: 5 })
        ),
        "{result:?}"
    );
    Ok(())
}
