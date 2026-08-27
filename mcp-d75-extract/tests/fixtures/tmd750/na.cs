public class na
{
	private List<nb> l;

	public void ai()
	{
		int num4 = 0;
		while (num4 < 6)
		{
			l.Add(new nb());
			l[num4].OffsetProgrammableMemoryAddress = 8192 * num4;
			num4++;
		}
	}

	public void a6(n7 A_0)
	{
		int num4 = 0;
		while (num4 < 6)
		{
			l[num4].a6(A_0);
			num4++;
		}
	}

	public void a7(n7 A_0)
	{
		l[0].a7(A_0);
	}
}
